/*
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * Minimal SHA-256 (FIPS 180-4) used to verify the downloaded target image.
 * Self-contained so it can be unit tested on the host and does not depend on
 * which crypto library the Mongoose OS build happens to link.
 */
#ifndef MG2X_SHA256_H
#define MG2X_SHA256_H

#include <stddef.h>
#include <stdint.h>

#define MG2X_SHA256_DIGEST_BYTES 32u
#define MG2X_SHA256_BLOCK_BYTES 64u

typedef struct {
  uint32_t state[8];
  uint64_t total_bytes;
  uint8_t block[MG2X_SHA256_BLOCK_BYTES];
  size_t block_fill_bytes;
} mg2x_sha256_ctx;

void mg2x_sha256_init(mg2x_sha256_ctx *ctx);
void mg2x_sha256_update(mg2x_sha256_ctx *ctx, const uint8_t *data, size_t len);
void mg2x_sha256_final(mg2x_sha256_ctx *ctx, uint8_t digest[MG2X_SHA256_DIGEST_BYTES]);

#endif /* MG2X_SHA256_H */
