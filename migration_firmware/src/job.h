/*
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * Validation of a flash job: the parameters handed over by the CLI and the
 * stream of downloaded bytes. Hardware independent so it can be unit tested on
 * the host. The firmware only copies the image to address 0x0 after
 * mg2x_job_finish() returned MG2X_JOB_OK.
 */
#ifndef MG2X_JOB_H
#define MG2X_JOB_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "sha256.h"

/* First byte of every ESP8266 application image. */
#define MG2X_IMAGE_MAGIC 0xE9u
#define MG2X_SHA256_HEX_CHARS 64u

typedef enum {
  MG2X_JOB_OK = 0,
  MG2X_JOB_ERR_URL_SCHEME,
  MG2X_JOB_ERR_SIZE_TOO_SMALL,
  MG2X_JOB_ERR_SIZE_TOO_LARGE,
  MG2X_JOB_ERR_HASH_FORMAT,
  MG2X_JOB_ERR_OVERFLOW,
  MG2X_JOB_ERR_BAD_MAGIC,
  MG2X_JOB_ERR_TRUNCATED,
  MG2X_JOB_ERR_HASH_MISMATCH
} mg2x_job_result;

typedef struct {
  uint32_t expected_size_bytes;
  uint32_t received_bytes;
  uint8_t expected_sha256[MG2X_SHA256_DIGEST_BYTES];
  bool magic_checked;
  mg2x_sha256_ctx sha;
} mg2x_job;

/*
 * Validates the parameters and resets the job. `url` must use plain http
 * (the ESP8266 has too little RAM for TLS during this step). The size must lie
 * within [min_size_bytes, max_size_bytes].
 */
mg2x_job_result mg2x_job_begin(mg2x_job *job, const char *url, uint32_t size_bytes,
                               uint32_t min_size_bytes, uint32_t max_size_bytes,
                               const char *sha256_hex);

/* Feeds downloaded bytes. Fails on more bytes than announced or a bad magic. */
mg2x_job_result mg2x_job_feed(mg2x_job *job, const uint8_t *data, size_t len);

/* Checks completeness and the SHA-256 digest. */
mg2x_job_result mg2x_job_finish(mg2x_job *job);

/* Stable, machine-readable identifier of a result (snake_case). */
const char *mg2x_job_result_str(mg2x_job_result result);

#endif /* MG2X_JOB_H */
