// Loaded-image table. Filled outside signal context; read (lock-free) by the crash handler.
#if defined(__linux__)
#define _GNU_SOURCE
#endif

#include "VakaNative.h"

#include <dlfcn.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define VAKA_MAX_IMAGES 2048

static vaka_image_t g_images[VAKA_MAX_IMAGES];
static volatile int g_image_count = 0;
static volatile int g_images_initialized = 0;

static void push_image(const vaka_image_t *img) {
    int i = __sync_fetch_and_add(&g_image_count, 0);
    if (i >= VAKA_MAX_IMAGES) return;
    g_images[i] = *img;
    __sync_synchronize();
    __sync_fetch_and_add(&g_image_count, 1);
}

#if defined(__APPLE__)
#include <mach-o/dyld.h>
#include <mach-o/loader.h>

static void on_add_image(const struct mach_header *mh, intptr_t slide) {
    (void)slide;
    if (mh == NULL || mh->magic != MH_MAGIC_64) return;
    const struct mach_header_64 *h = (const struct mach_header_64 *)mh;
    vaka_image_t img;
    memset(&img, 0, sizeof img);
    img.kind = VAKA_IMAGE_MACHO;
    img.addr = (uint64_t)(uintptr_t)mh;
    uintptr_t cmd = (uintptr_t)(h + 1);
    for (uint32_t i = 0; i < h->ncmds; i++) {
        const struct load_command *lc = (const struct load_command *)cmd;
        if (lc->cmd == LC_SEGMENT_64) {
            const struct segment_command_64 *seg = (const struct segment_command_64 *)lc;
            if (strcmp(seg->segname, "__TEXT") == 0) img.size = seg->vmsize;
        } else if (lc->cmd == LC_UUID) {
            const struct uuid_command *u = (const struct uuid_command *)lc;
            memcpy(img.id, u->uuid, 16);
            img.id_len = 16;
        }
        cmd += lc->cmdsize;
    }
    Dl_info info;
    if (dladdr(mh, &info) && info.dli_fname) img.path = strdup(info.dli_fname);
    else img.path = "";
    push_image(&img);
}

void vaka_images_init(void) {
    if (!__sync_bool_compare_and_swap(&g_images_initialized, 0, 1)) return;
    // Called for every image already loaded, then for each one loaded later.
    _dyld_register_func_for_add_image(on_add_image);
}

#elif defined(__linux__)
#include <elf.h>
#include <link.h>

static int on_phdr(struct dl_phdr_info *info, size_t size, void *data) {
    (void)size; (void)data;
    vaka_image_t img;
    memset(&img, 0, sizeof img);
    img.kind = VAKA_IMAGE_ELF;
    uint64_t lo = UINT64_MAX, hi = 0;
    for (int i = 0; i < info->dlpi_phnum; i++) {
        const ElfW(Phdr) *ph = &info->dlpi_phdr[i];
        if (ph->p_type == PT_LOAD) {
            if (ph->p_vaddr < lo) lo = ph->p_vaddr;
            if (ph->p_vaddr + ph->p_memsz > hi) hi = ph->p_vaddr + ph->p_memsz;
        } else if (ph->p_type == PT_NOTE && img.id_len == 0) {
            const uint8_t *p = (const uint8_t *)(info->dlpi_addr + ph->p_vaddr);
            const uint8_t *end = p + ph->p_memsz;
            while (p + sizeof(ElfW(Nhdr)) <= end) {
                const ElfW(Nhdr) *n = (const ElfW(Nhdr) *)p;
                const uint8_t *name = p + sizeof(ElfW(Nhdr));
                const uint8_t *desc = name + ((n->n_namesz + 3) & ~3u);
                if (desc + n->n_descsz > end) break;
                if (n->n_type == NT_GNU_BUILD_ID && n->n_namesz == 4 && memcmp(name, "GNU", 4) == 0) {
                    uint32_t len = n->n_descsz > 32 ? 32 : n->n_descsz;
                    memcpy(img.id, desc, len);
                    img.id_len = len;
                    break;
                }
                p = desc + ((n->n_descsz + 3) & ~3u);
            }
        }
    }
    if (lo == UINT64_MAX) return 0;
    img.addr = info->dlpi_addr + lo;
    img.size = hi - lo;
    if (info->dlpi_name && info->dlpi_name[0]) {
        img.path = strdup(info->dlpi_name);
    } else {
        char buf[4096];
        ssize_t n = readlink("/proc/self/exe", buf, sizeof buf - 1);
        if (n > 0) { buf[n] = 0; img.path = strdup(buf); } else img.path = "";
    }
    push_image(&img);
    return 0;
}

void vaka_images_init(void) {
    if (!__sync_bool_compare_and_swap(&g_images_initialized, 0, 1)) return;
    dl_iterate_phdr(on_phdr, NULL);
}

#else
void vaka_images_init(void) {}
#endif

int vaka_image_count(void) {
    return __sync_fetch_and_add(&g_image_count, 0);
}

int vaka_image_get(int index, vaka_image_t *out) {
    if (index < 0 || index >= vaka_image_count() || out == NULL) return -1;
    *out = g_images[index];
    return 0;
}

int vaka_image_find(uint64_t addr) {
    int n = vaka_image_count();
    int best = -1;
    for (int i = 0; i < n; i++) {
        const vaka_image_t *img = &g_images[i];
        if (addr >= img->addr && addr < img->addr + img->size) {
            if (best < 0 || img->addr > g_images[best].addr) best = i;
        }
    }
    return best;
}

const char *vaka_symbol_name(uintptr_t addr, uintptr_t *symbol_addr) {
    Dl_info info;
    memset(&info, 0, sizeof info);
    if (!dladdr((const void *)addr, &info) || !info.dli_sname) return NULL;
    if (symbol_addr) *symbol_addr = (uintptr_t)info.dli_saddr;
    return info.dli_sname;
}
