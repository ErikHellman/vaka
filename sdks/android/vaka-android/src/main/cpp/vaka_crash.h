// Vaka native crash handler. Platform code only (no JNI), so it can be tested on a desktop.
#ifndef VAKA_CRASH_H
#define VAKA_CRASH_H

#include <stdbool.h>

// Installs signal handlers that write a crash report into `dir` (created by the caller).
// Returns false if the handler could not be installed. Safe to call once.
bool vaka_crash_install(const char *dir, const char *release);

// Current session id, written into the report so the next launch can mark that session crashed.
void vaka_crash_set_session(const char *session_id);

#endif
