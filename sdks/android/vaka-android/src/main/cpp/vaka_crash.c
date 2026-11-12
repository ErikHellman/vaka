// Vaka native crash handler.
//
// On a fatal signal it writes a small text report (no minidump) into the crash directory:
// signal, code, fault address, the backtrace as raw addresses (crash site first) and the loaded
// ELF images with their GNU build ids. The Kotlin side turns it into an error item on the next
// launch and the server symbolicates the addresses with uploaded debug files.
//
// Everything on the signal path avoids malloc, stdio and locks we own. Two calls are not
// formally async-signal-safe but are what native crash reporters commonly rely on:
// _Unwind_Backtrace (the unwinder steps through the kernel's signal frame) and
// dl_iterate_phdr (runs last, so a deadlock there still leaves a usable report on disk).
//
// Report format, one record per line:
//   vaka-native-crash 1
//   timestamp_ms <decimal>      signal <decimal>     code <decimal>     fault_addr <hex>
//   pid / tid <decimal>         thread <name>        session <id>       release <text>
//   frame <hex address>         (repeated, crash site first)
//   image <hex start> <hex size> <build id hex or -> <path>   (repeated)
//   end

#define _GNU_SOURCE
#include "vaka_crash.h"

#include <errno.h>
#include <fcntl.h>
#include <link.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>
#include <unwind.h>

#define MAX_FRAMES 128
#define PATH_LEN 512
#define FIELD_LEN 128

static const int SIGNALS[] = {SIGSEGV, SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGTRAP, SIGSYS};
#define NSIGNALS ((int)(sizeof(SIGNALS) / sizeof(SIGNALS[0])))

static struct sigaction g_previous[NSIGNALS];
static char g_dir[PATH_LEN];
static char g_release[FIELD_LEN];
static char g_session[FIELD_LEN];
static atomic_int g_handling = 0;
static bool g_installed = false;

// ------------------------------------------------------------------ async-signal-safe output

typedef struct {
    int fd;
    size_t len;
    char buf[1024];
} writer;

static void w_flush(writer *w) {
    size_t off = 0;
    while (off < w->len) {
        ssize_t n = write(w->fd, w->buf + off, w->len - off);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        off += (size_t)n;
    }
    w->len = 0;
}

static void w_char(writer *w, char c) {
    if (w->len == sizeof(w->buf)) w_flush(w);
    w->buf[w->len++] = c;
}

static void w_str(writer *w, const char *s) {
    for (; *s; s++) w_char(w, *s);
}

// A value inside a record: newlines would break the line format.
static void w_value(writer *w, const char *s) {
    for (; *s; s++) w_char(w, (*s == '\n' || *s == '\r') ? ' ' : *s);
}

static void w_hex(writer *w, uintptr_t v) {
    char tmp[2 + sizeof(uintptr_t) * 2];
    int n = 0;
    do {
        tmp[n++] = "0123456789abcdef"[v & 0xf];
        v >>= 4;
    } while (v);
    w_str(w, "0x");
    while (n) w_char(w, tmp[--n]);
}

static void w_dec(writer *w, long long v) {
    char tmp[24];
    int n = 0;
    unsigned long long u = v < 0 ? (unsigned long long)(-(v + 1)) + 1 : (unsigned long long)v;
    if (v < 0) w_char(w, '-');
    do {
        tmp[n++] = (char)('0' + u % 10);
        u /= 10;
    } while (u);
    while (n) w_char(w, tmp[--n]);
}

static void w_field(writer *w, const char *key) {
    w_str(w, key);
    w_char(w, ' ');
}

static void copy_str(char *dst, const char *src, size_t cap) {
    size_t i = 0;
    if (src)
        for (; src[i] && i + 1 < cap; i++) dst[i] = src[i];
    dst[i] = 0;
}

// ------------------------------------------------------------------ registers and unwinding

static uintptr_t context_pc(const ucontext_t *uc) {
#if defined(__aarch64__)
    return (uintptr_t)uc->uc_mcontext.pc;
#elif defined(__arm__)
    return (uintptr_t)uc->uc_mcontext.arm_pc;
#elif defined(__x86_64__)
    return (uintptr_t)uc->uc_mcontext.gregs[REG_RIP];
#elif defined(__i386__)
    return (uintptr_t)uc->uc_mcontext.gregs[REG_EIP];
#else
    (void)uc;
    return 0;
#endif
}

static uintptr_t context_lr(const ucontext_t *uc) {
#if defined(__aarch64__)
    return (uintptr_t)uc->uc_mcontext.regs[30];
#elif defined(__arm__)
    return (uintptr_t)uc->uc_mcontext.arm_lr;
#else
    (void)uc;
    return 0;
#endif
}

typedef struct {
    uintptr_t frames[MAX_FRAMES];
    int count;
} backtrace_state;

static _Unwind_Reason_Code unwind_cb(struct _Unwind_Context *ctx, void *arg) {
    backtrace_state *s = (backtrace_state *)arg;
    uintptr_t ip = (uintptr_t)_Unwind_GetIP(ctx);
    if (ip == 0) return _URC_END_OF_STACK;
    s->frames[s->count++] = ip;
    return s->count >= MAX_FRAMES ? _URC_END_OF_STACK : _URC_NO_REASON;
}

// The unwinder starts inside this handler: drop frames up to the interrupted pc. If the unwinder
// cannot step through the signal frame, fall back to pc (+ link register where there is one).
static int collect_frames(const ucontext_t *uc, uintptr_t *out) {
    backtrace_state s;
    s.count = 0;
    uintptr_t pc = uc ? context_pc(uc) : 0;
    _Unwind_Backtrace(unwind_cb, &s);
    int start = -1;
    for (int i = 0; i < s.count && pc; i++) {
        if (s.frames[i] == pc || s.frames[i] == pc + 1) {
            start = i;
            break;
        }
    }
    int n = 0;
    if (start >= 0) {
        out[n++] = pc;
        for (int i = start + 1; i < s.count && n < MAX_FRAMES; i++) out[n++] = s.frames[i];
    } else if (pc) {
        out[n++] = pc;
        uintptr_t lr = context_lr(uc);
        if (lr) out[n++] = lr;
    } else {
        for (int i = 0; i < s.count && n < MAX_FRAMES; i++) out[n++] = s.frames[i];
    }
    return n;
}

// ------------------------------------------------------------------ loaded images

static int image_cb(struct dl_phdr_info *info, size_t size, void *arg) {
    (void)size;
    writer *w = (writer *)arg;
    uintptr_t lo = UINTPTR_MAX, hi = 0;
    const unsigned char *build_id = NULL;
    size_t build_id_len = 0;
    for (int i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
        if (ph->p_type == PT_LOAD) {
            uintptr_t start = (uintptr_t)ph->p_vaddr;
            uintptr_t end = start + (uintptr_t)ph->p_memsz;
            if (start < lo) lo = start;
            if (end > hi) hi = end;
        } else if (ph->p_type == PT_NOTE && !build_id) {
            const unsigned char *p = (const unsigned char *)(info->dlpi_addr + ph->p_vaddr);
            const unsigned char *end = p + ph->p_memsz;
            while (p + sizeof(ElfW(Nhdr)) <= end) {
                const ElfW(Nhdr) *nh = (const ElfW(Nhdr) *)p;
                const unsigned char *name = p + sizeof(ElfW(Nhdr));
                const unsigned char *desc = name + ((nh->n_namesz + 3) & ~3u);
                const unsigned char *next = desc + ((nh->n_descsz + 3) & ~3u);
                if (next > end) break;
                if (nh->n_type == NT_GNU_BUILD_ID && nh->n_namesz == 4 && memcmp(name, "GNU", 4) == 0) {
                    build_id = desc;
                    build_id_len = nh->n_descsz;
                    break;
                }
                p = next;
            }
        }
    }
    if (hi <= lo) return 0;
    lo &= ~(uintptr_t)0xfff;
    w_field(w, "image");
    w_hex(w, (uintptr_t)info->dlpi_addr + lo);
    w_char(w, ' ');
    w_hex(w, hi - lo);
    w_char(w, ' ');
    if (build_id && build_id_len) {
        for (size_t i = 0; i < build_id_len; i++) {
            w_char(w, "0123456789abcdef"[build_id[i] >> 4]);
            w_char(w, "0123456789abcdef"[build_id[i] & 0xf]);
        }
    } else {
        w_char(w, '-');
    }
    w_char(w, ' ');
    w_value(w, info->dlpi_name && info->dlpi_name[0] ? info->dlpi_name : "-");
    w_char(w, '\n');
    return 0;
}

// ------------------------------------------------------------------ the handler

static void write_report(int sig, siginfo_t *info, ucontext_t *uc) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    long long now_ms = (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;

    // <dir>/<timestamp>.crash
    char path[PATH_LEN + 32];
    writer w;
    w.len = 0;
    w.fd = -1;
    size_t n = 0;
    for (; g_dir[n] && n < PATH_LEN; n++) path[n] = g_dir[n];
    path[n++] = '/';
    {
        char digits[24];
        int d = 0;
        long long v = now_ms;
        do {
            digits[d++] = (char)('0' + v % 10);
            v /= 10;
        } while (v);
        while (d) path[n++] = digits[--d];
    }
    memcpy(path + n, ".crash", 7);

    w.fd = open(path, O_CREAT | O_WRONLY | O_TRUNC | O_CLOEXEC, 0600);
    if (w.fd < 0) return;

    w_str(&w, "vaka-native-crash 1\n");
    w_field(&w, "timestamp_ms"); w_dec(&w, now_ms); w_char(&w, '\n');
    w_field(&w, "signal"); w_dec(&w, sig); w_char(&w, '\n');
    w_field(&w, "code"); w_dec(&w, info ? info->si_code : 0); w_char(&w, '\n');
    w_field(&w, "fault_addr"); w_hex(&w, info ? (uintptr_t)info->si_addr : 0); w_char(&w, '\n');
    w_field(&w, "pid"); w_dec(&w, getpid()); w_char(&w, '\n');
    w_field(&w, "tid"); w_dec(&w, (long long)syscall(SYS_gettid)); w_char(&w, '\n');
    char thread[17] = {0};
    if (prctl(PR_GET_NAME, thread, 0, 0, 0) == 0 && thread[0]) {
        w_field(&w, "thread"); w_value(&w, thread); w_char(&w, '\n');
    }
    if (g_session[0]) { w_field(&w, "session"); w_value(&w, g_session); w_char(&w, '\n'); }
    if (g_release[0]) { w_field(&w, "release"); w_value(&w, g_release); w_char(&w, '\n'); }

    uintptr_t frames[MAX_FRAMES];
    int count = collect_frames(uc, frames);
    for (int i = 0; i < count; i++) {
        w_field(&w, "frame"); w_hex(&w, frames[i]); w_char(&w, '\n');
    }
    // Everything above is on disk even if the image walk below goes wrong.
    w_flush(&w);
    dl_iterate_phdr(image_cb, &w);
    w_str(&w, "end\n");
    w_flush(&w);
    close(w.fd);
}

static int signal_index(int sig) {
    for (int i = 0; i < NSIGNALS; i++)
        if (SIGNALS[i] == sig) return i;
    return -1;
}

static void restore_previous(void) {
    for (int i = 0; i < NSIGNALS; i++) sigaction(SIGNALS[i], &g_previous[i], NULL);
}

static void handler(int sig, siginfo_t *info, void *ctx) {
    int expected = 0;
    if (atomic_compare_exchange_strong(&g_handling, &expected, 1)) {
        int saved = errno;
        write_report(sig, info, (ucontext_t *)ctx);
        errno = saved;
    }
    // Hand over to whatever was installed before us (debuggerd on Android writes the tombstone):
    // with the old handlers back, re-raise the signal on this thread. It is blocked while we run,
    // so it is delivered as soon as we return; a hardware fault would re-trigger anyway.
    restore_previous();
    int idx = signal_index(sig);
    if (idx >= 0 && (g_previous[idx].sa_flags & SA_SIGINFO) && g_previous[idx].sa_sigaction) {
        g_previous[idx].sa_sigaction(sig, info, ctx);
        return;
    }
    syscall(SYS_tgkill, getpid(), (pid_t)syscall(SYS_gettid), sig);
}

bool vaka_crash_install(const char *dir, const char *release) {
    if (g_installed) return true;
    if (!dir || !dir[0]) return false;
    copy_str(g_dir, dir, sizeof(g_dir));
    copy_str(g_release, release, sizeof(g_release));

    // Stack overflows need an alternate signal stack. Bionic gives every thread one already;
    // only add one for this thread if none is set (e.g. when testing on a desktop).
    stack_t current;
    if (sigaltstack(NULL, &current) == 0 && (current.ss_flags & SS_DISABLE)) {
        static char alt_stack[64 * 1024];
        stack_t ss;
        ss.ss_sp = alt_stack;
        ss.ss_size = sizeof(alt_stack);
        ss.ss_flags = 0;
        sigaltstack(&ss, NULL);
    }

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sigemptyset(&sa.sa_mask);
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    for (int i = 0; i < NSIGNALS; i++) {
        if (sigaction(SIGNALS[i], &sa, &g_previous[i]) != 0) {
            for (int j = 0; j < i; j++) sigaction(SIGNALS[j], &g_previous[j], NULL);
            return false;
        }
    }
    g_installed = true;
    return true;
}

void vaka_crash_set_session(const char *session_id) {
    // Not atomic with respect to a crash on another thread; the reader validates the value.
    if (!session_id) {
        g_session[0] = 0;
        return;
    }
    char tmp[FIELD_LEN];
    copy_str(tmp, session_id, sizeof(tmp));
    memcpy(g_session, tmp, sizeof(tmp));
}
