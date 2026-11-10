/*
 * SPDX-License-Identifier: GPL-3.0-only
 */
#include "mg2x_job.h"

#include <string.h>

static int hex_value(char c) {
  if (c >= '0' && c <= '9') {
    return c - '0';
  }
  if (c >= 'a' && c <= 'f') {
    return c - 'a' + 10;
  }
  if (c >= 'A' && c <= 'F') {
    return c - 'A' + 10;
  }
  return -1;
}

static bool parse_sha256_hex(const char *hex, uint8_t out[MG2X_SHA256_DIGEST_BYTES]) {
  if (hex == NULL || strlen(hex) != MG2X_SHA256_HEX_CHARS) {
    return false;
  }
  for (size_t i = 0; i < MG2X_SHA256_DIGEST_BYTES; i++) {
    int hi = hex_value(hex[2u * i]);
    int lo = hex_value(hex[2u * i + 1u]);
    if (hi < 0 || lo < 0) {
      return false;
    }
    out[i] = (uint8_t)((hi << 4) | lo);
  }
  return true;
}

mg2x_job_result mg2x_job_begin(mg2x_job *job, const char *url, uint32_t size_bytes,
                               uint32_t min_size_bytes, uint32_t max_size_bytes,
                               const char *sha256_hex) {
  static const char scheme[] = "http://";
  if (url == NULL || strncmp(url, scheme, sizeof(scheme) - 1u) != 0 ||
      strlen(url) <= sizeof(scheme) - 1u) {
    return MG2X_JOB_ERR_URL_SCHEME;
  }
  if (size_bytes < min_size_bytes) {
    return MG2X_JOB_ERR_SIZE_TOO_SMALL;
  }
  if (size_bytes > max_size_bytes) {
    return MG2X_JOB_ERR_SIZE_TOO_LARGE;
  }
  uint8_t digest[MG2X_SHA256_DIGEST_BYTES];
  if (!parse_sha256_hex(sha256_hex, digest)) {
    return MG2X_JOB_ERR_HASH_FORMAT;
  }

  memset(job, 0, sizeof(*job));
  job->expected_size_bytes = size_bytes;
  memcpy(job->expected_sha256, digest, sizeof(digest));
  mg2x_sha256_init(&job->sha);
  return MG2X_JOB_OK;
}

mg2x_job_result mg2x_job_feed(mg2x_job *job, const uint8_t *data, size_t len) {
  if (len == 0u) {
    return MG2X_JOB_OK;
  }
  uint32_t remaining = job->expected_size_bytes - job->received_bytes;
  if (len > (size_t)remaining) {
    return MG2X_JOB_ERR_OVERFLOW;
  }
  if (!job->magic_checked) {
    if (data[0] != MG2X_IMAGE_MAGIC) {
      return MG2X_JOB_ERR_BAD_MAGIC;
    }
    job->magic_checked = true;
  }
  mg2x_sha256_update(&job->sha, data, len);
  job->received_bytes += (uint32_t)len;
  return MG2X_JOB_OK;
}

mg2x_job_result mg2x_job_finish(mg2x_job *job) {
  if (job->received_bytes != job->expected_size_bytes) {
    return MG2X_JOB_ERR_TRUNCATED;
  }
  uint8_t digest[MG2X_SHA256_DIGEST_BYTES];
  mg2x_sha256_final(&job->sha, digest);
  if (memcmp(digest, job->expected_sha256, sizeof(digest)) != 0) {
    return MG2X_JOB_ERR_HASH_MISMATCH;
  }
  return MG2X_JOB_OK;
}

const char *mg2x_job_result_str(mg2x_job_result result) {
  switch (result) {
  case MG2X_JOB_OK:
    return "ok";
  case MG2X_JOB_ERR_URL_SCHEME:
    return "url_scheme";
  case MG2X_JOB_ERR_SIZE_TOO_SMALL:
    return "size_too_small";
  case MG2X_JOB_ERR_SIZE_TOO_LARGE:
    return "size_too_large";
  case MG2X_JOB_ERR_HASH_FORMAT:
    return "hash_format";
  case MG2X_JOB_ERR_OVERFLOW:
    return "overflow";
  case MG2X_JOB_ERR_BAD_MAGIC:
    return "bad_magic";
  case MG2X_JOB_ERR_TRUNCATED:
    return "truncated";
  case MG2X_JOB_ERR_HASH_MISMATCH:
    return "hash_mismatch";
  }
  return "unknown";
}
