// Desktop test harness for vaka_crash.c: installs the handler, then crashes (0 = SIGSEGV, 1 = abort).
#include "vaka_crash.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
__attribute__((noinline)) void crash_here(int kind) {
    if (kind == 1) abort();
    volatile int *p = NULL; *p = 1;
}
__attribute__((noinline)) void caller(int kind) { crash_here(kind); printf("x\n"); }
int main(int argc, char **argv) {
    vaka_crash_install(argv[1], "1.2.3");
    vaka_crash_set_session("11111111-2222-3333-4444-555555555555");
    caller(argc > 2 ? atoi(argv[2]) : 0);
    return 0;
}
