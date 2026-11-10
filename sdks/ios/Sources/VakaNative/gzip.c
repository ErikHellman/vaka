#include "VakaNative.h"

#include <string.h>
#include <zlib.h>

size_t vaka_gzip_bound(size_t len) {
    return compressBound((uLong)len) + 32;
}

size_t vaka_gzip(const uint8_t *in, size_t len, uint8_t *out, size_t cap) {
    z_stream s;
    memset(&s, 0, sizeof s);
    // windowBits 15 + 16 selects the gzip wrapper.
    if (deflateInit2(&s, Z_DEFAULT_COMPRESSION, Z_DEFLATED, 15 + 16, 8, Z_DEFAULT_STRATEGY) != Z_OK) return 0;
    s.next_in = (Bytef *)in;
    s.avail_in = (uInt)len;
    s.next_out = out;
    s.avail_out = (uInt)cap;
    int rc = deflate(&s, Z_FINISH);
    size_t produced = cap - s.avail_out;
    deflateEnd(&s);
    return rc == Z_STREAM_END ? produced : 0;
}
