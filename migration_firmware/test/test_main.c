/*
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * Host unit tests for the hardware independent parts of the migration
 * firmware. Build and run with `make -C migration_firmware/test check`.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../src/mg2x_install.h"
#include "../src/mg2x_job.h"
#include "../src/mg2x_sha256.h"
#include "../src/mg2x_writer.h"

static int s_failures = 0;

#define CHECK(cond)                                                                                \
  do {                                                                                             \
    if (!(cond)) {                                                                                 \
      fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #cond);                     \
      s_failures++;                                                                                \
    }                                                                                              \
  } while (0)

static void to_hex(const uint8_t *digest, char out[65]) {
  static const char digits[] = "0123456789abcdef";
  for (size_t i = 0; i < 32u; i++) {
    out[2u * i] = digits[digest[i] >> 4];
    out[2u * i + 1u] = digits[digest[i] & 0x0Fu];
  }
  out[64] = '\0';
}

static void sha256_hex(const uint8_t *data, size_t len, char out[65]) {
  mg2x_sha256_ctx ctx;
  uint8_t digest[32];
  mg2x_sha256_init(&ctx);
  mg2x_sha256_update(&ctx, data, len);
  mg2x_sha256_final(&ctx, digest);
  to_hex(digest, out);
}

static void test_sha256_vectors(void) {
  char hex[65];
  sha256_hex((const uint8_t *)"", 0, hex);
  CHECK(strcmp(hex, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855") == 0);
  sha256_hex((const uint8_t *)"abc", 3, hex);
  CHECK(strcmp(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad") == 0);
  const char *two_blocks = "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
  sha256_hex((const uint8_t *)two_blocks, strlen(two_blocks), hex);
  CHECK(strcmp(hex, "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1") == 0);
}

static void test_sha256_incremental_matches_one_shot(void) {
  uint8_t data[1000];
  for (size_t i = 0; i < sizeof(data); i++) {
    data[i] = (uint8_t)(i * 7u);
  }
  char one_shot[65];
  sha256_hex(data, sizeof(data), one_shot);

  mg2x_sha256_ctx ctx;
  uint8_t digest[32];
  char incremental[65];
  mg2x_sha256_init(&ctx);
  size_t offset = 0;
  size_t step = 1;
  while (offset < sizeof(data)) {
    size_t take = sizeof(data) - offset < step ? sizeof(data) - offset : step;
    mg2x_sha256_update(&ctx, &data[offset], take);
    offset += take;
    step = step * 3u + 1u;
  }
  mg2x_sha256_final(&ctx, digest);
  to_hex(digest, incremental);
  CHECK(strcmp(one_shot, incremental) == 0);
}

/* ---- job ---------------------------------------------------------------- */

static uint8_t s_image[10000];
static char s_image_sha[65];

static void make_image(void) {
  for (size_t i = 0; i < sizeof(s_image); i++) {
    s_image[i] = (uint8_t)(i % 251u);
  }
  s_image[0] = MG2X_IMAGE_MAGIC;
  sha256_hex(s_image, sizeof(s_image), s_image_sha);
}

static const char *URL = "http://10.10.10.96:8266/firmware.bin";

static void test_job_begin_validation(void) {
  mg2x_job job;
  CHECK(mg2x_job_begin(&job, NULL, 100, 1, 1000, s_image_sha) == MG2X_JOB_ERR_URL_SCHEME);
  CHECK(mg2x_job_begin(&job, "https://x/y", 100, 1, 1000, s_image_sha) == MG2X_JOB_ERR_URL_SCHEME);
  CHECK(mg2x_job_begin(&job, "http://", 100, 1, 1000, s_image_sha) == MG2X_JOB_ERR_URL_SCHEME);
  CHECK(mg2x_job_begin(&job, URL, 0, 1, 1000, s_image_sha) == MG2X_JOB_ERR_SIZE_TOO_SMALL);
  CHECK(mg2x_job_begin(&job, URL, 1001, 1, 1000, s_image_sha) == MG2X_JOB_ERR_SIZE_TOO_LARGE);
  CHECK(mg2x_job_begin(&job, URL, 100, 1, 1000, NULL) == MG2X_JOB_ERR_HASH_FORMAT);
  CHECK(mg2x_job_begin(&job, URL, 100, 1, 1000, "abc") == MG2X_JOB_ERR_HASH_FORMAT);
  char bad[65];
  memcpy(bad, s_image_sha, sizeof(bad));
  bad[10] = 'g';
  CHECK(mg2x_job_begin(&job, URL, 100, 1, 1000, bad) == MG2X_JOB_ERR_HASH_FORMAT);
  bad[10] = '0';
  bad[11] = 'Z';
  CHECK(mg2x_job_begin(&job, URL, 100, 1, 1000, bad) == MG2X_JOB_ERR_HASH_FORMAT);
  bad[11] = '!'; /* below '0' */
  CHECK(mg2x_job_begin(&job, URL, 100, 1, 1000, bad) == MG2X_JOB_ERR_HASH_FORMAT);
  bad[11] = ':'; /* between '9' and 'A' */
  CHECK(mg2x_job_begin(&job, URL, 100, 1, 1000, bad) == MG2X_JOB_ERR_HASH_FORMAT);
}

static void test_job_accepts_uppercase_hash_and_streams(void) {
  char upper[65];
  for (size_t i = 0; i < 65u; i++) {
    char c = s_image_sha[i];
    upper[i] = (c >= 'a' && c <= 'f') ? (char)(c - 'a' + 'A') : c;
  }
  mg2x_job job;
  CHECK(mg2x_job_begin(&job, URL, sizeof(s_image), 1, sizeof(s_image), upper) == MG2X_JOB_OK);
  CHECK(mg2x_job_feed(&job, s_image, 0) == MG2X_JOB_OK);
  size_t offset = 0;
  while (offset < sizeof(s_image)) {
    size_t take = sizeof(s_image) - offset < 1460u ? sizeof(s_image) - offset : 1460u;
    CHECK(mg2x_job_feed(&job, &s_image[offset], take) == MG2X_JOB_OK);
    offset += take;
  }
  CHECK(mg2x_job_finish(&job) == MG2X_JOB_OK);
}

static void test_job_rejects_bad_streams(void) {
  mg2x_job job;
  /* more bytes than announced */
  CHECK(mg2x_job_begin(&job, URL, 100, 1, 1000, s_image_sha) == MG2X_JOB_OK);
  CHECK(mg2x_job_feed(&job, s_image, 101) == MG2X_JOB_ERR_OVERFLOW);
  /* wrong magic byte */
  uint8_t not_an_image[4] = {0x00, 0x01, 0x02, 0x03};
  CHECK(mg2x_job_begin(&job, URL, 4, 1, 1000, s_image_sha) == MG2X_JOB_OK);
  CHECK(mg2x_job_feed(&job, not_an_image, 4) == MG2X_JOB_ERR_BAD_MAGIC);
  /* transfer cut off */
  CHECK(mg2x_job_begin(&job, URL, sizeof(s_image), 1, sizeof(s_image), s_image_sha) == MG2X_JOB_OK);
  CHECK(mg2x_job_feed(&job, s_image, 5000) == MG2X_JOB_OK);
  CHECK(mg2x_job_finish(&job) == MG2X_JOB_ERR_TRUNCATED);
  /* corrupted byte */
  CHECK(mg2x_job_begin(&job, URL, sizeof(s_image), 1, sizeof(s_image), s_image_sha) == MG2X_JOB_OK);
  CHECK(mg2x_job_feed(&job, s_image, 5000) == MG2X_JOB_OK);
  uint8_t corrupted = (uint8_t)(s_image[5000] ^ 0x01u);
  CHECK(mg2x_job_feed(&job, &corrupted, 1) == MG2X_JOB_OK);
  CHECK(mg2x_job_feed(&job, &s_image[5001], sizeof(s_image) - 5001u) == MG2X_JOB_OK);
  CHECK(mg2x_job_finish(&job) == MG2X_JOB_ERR_HASH_MISMATCH);
}

static void test_job_result_strings(void) {
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_OK), "ok") == 0);
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_ERR_URL_SCHEME), "url_scheme") == 0);
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_ERR_SIZE_TOO_SMALL), "size_too_small") == 0);
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_ERR_SIZE_TOO_LARGE), "size_too_large") == 0);
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_ERR_HASH_FORMAT), "hash_format") == 0);
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_ERR_OVERFLOW), "overflow") == 0);
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_ERR_BAD_MAGIC), "bad_magic") == 0);
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_ERR_TRUNCATED), "truncated") == 0);
  CHECK(strcmp(mg2x_job_result_str(MG2X_JOB_ERR_HASH_MISMATCH), "hash_mismatch") == 0);
  CHECK(strcmp(mg2x_job_result_str((mg2x_job_result)99), "unknown") == 0);
}

/* ---- writer ------------------------------------------------------------- */

#define FAKE_FLASH_BYTES (64u * MG2X_SECTOR_BYTES)

typedef struct {
  uint8_t bytes[FAKE_FLASH_BYTES];
  int fail_erase_at_call;
  int fail_write_at_call;
  int erase_calls;
  int write_calls;
  /* Generic failure injection over all operations (read, erase, write). */
  int ops;
  int fail_op_at;
  int fail_persistent;
  int fail_op_at_second;
} fake_flash;

static int op_fails(fake_flash *flash) {
  flash->ops++;
  if (flash->fail_op_at_second != 0 && flash->ops == flash->fail_op_at_second) {
    return 1;
  }
  if (flash->fail_op_at == 0) {
    return 0;
  }
  return flash->fail_persistent ? flash->ops >= flash->fail_op_at : flash->ops == flash->fail_op_at;
}

static int fake_read(void *ctx, uint32_t address, uint8_t *data, uint32_t len_bytes) {
  fake_flash *flash = ctx;
  if (op_fails(flash) || address + len_bytes > FAKE_FLASH_BYTES) {
    return 1;
  }
  memcpy(data, &flash->bytes[address], len_bytes);
  return 0;
}

static int fake_erase(void *ctx, uint32_t sector_index) {
  fake_flash *flash = ctx;
  if (op_fails(flash)) {
    return 1;
  }
  flash->erase_calls++;
  if (flash->erase_calls == flash->fail_erase_at_call) {
    return 1;
  }
  memset(&flash->bytes[sector_index * MG2X_SECTOR_BYTES], 0xFF, MG2X_SECTOR_BYTES);
  return 0;
}

static int fake_write(void *ctx, uint32_t address, const uint8_t *data, uint32_t len_bytes) {
  fake_flash *flash = ctx;
  if (op_fails(flash)) {
    return 1;
  }
  flash->write_calls++;
  if (flash->write_calls == flash->fail_write_at_call) {
    return 1;
  }
  if (len_bytes % 4u != 0u || address + len_bytes > FAKE_FLASH_BYTES) {
    return 1;
  }
  memcpy(&flash->bytes[address], data, len_bytes);
  return 0;
}

static fake_flash *new_flash(void) {
  fake_flash *flash = calloc(1, sizeof(*flash));
  if (flash == NULL) {
    abort();
  }
  return flash;
}

static mg2x_flash_ops ops_for(fake_flash *flash) {
  mg2x_flash_ops ops = {fake_read, fake_erase, fake_write, flash};
  return ops;
}

static void test_writer_init_alignment(void) {
  fake_flash *flash = new_flash();
  mg2x_writer *writer = malloc(sizeof(*writer));
  CHECK(writer != NULL);
  CHECK(mg2x_writer_init(writer, ops_for(flash), 1, 2u * MG2X_SECTOR_BYTES) ==
        MG2X_WRITER_ERR_ALIGNMENT);
  CHECK(mg2x_writer_init(writer, ops_for(flash), 0, 100) == MG2X_WRITER_ERR_ALIGNMENT);
  CHECK(mg2x_writer_init(writer, ops_for(flash), MG2X_SECTOR_BYTES, MG2X_SECTOR_BYTES) ==
        MG2X_WRITER_ERR_ALIGNMENT);
  CHECK(mg2x_writer_init(writer, ops_for(flash), MG2X_SECTOR_BYTES, 2u * MG2X_SECTOR_BYTES) ==
        MG2X_WRITER_OK);
  free(writer);
  free(flash);
}

static void test_writer_handles_any_chunk_size(void) {
  /* Chunk sizes from 1 byte up to more than two sectors in one call. */
  static const size_t steps[] = {1, 7, 1460, 4096, 4097, 9000, 3};
  fake_flash *flash = new_flash();
  mg2x_writer *writer = malloc(sizeof(*writer));
  CHECK(writer != NULL);
  uint32_t base = 4u * MG2X_SECTOR_BYTES;
  CHECK(mg2x_writer_init(writer, ops_for(flash), base, 16u * MG2X_SECTOR_BYTES) == MG2X_WRITER_OK);
  size_t offset = 0;
  size_t i = 0;
  while (offset < sizeof(s_image)) {
    size_t step = steps[i % (sizeof(steps) / sizeof(steps[0]))];
    size_t take = sizeof(s_image) - offset < step ? sizeof(s_image) - offset : step;
    CHECK(mg2x_writer_push(writer, &s_image[offset], take) == MG2X_WRITER_OK);
    offset += take;
    i++;
  }
  CHECK(mg2x_writer_flush(writer) == MG2X_WRITER_OK);
  CHECK(mg2x_writer_flush(writer) == MG2X_WRITER_OK); /* nothing pending */
  CHECK(memcmp(&flash->bytes[base], s_image, sizeof(s_image)) == 0);
  /* 10000 bytes -> 3 sectors; tail padded with 0xFF up to a word boundary. */
  CHECK(mg2x_writer_end_address(writer) == base + 3u * MG2X_SECTOR_BYTES);
  CHECK(flash->bytes[base + sizeof(s_image)] == 0xFF);
  free(writer);
  free(flash);
}

static void test_writer_pads_unaligned_tails(void) {
  /* Final blocks of 1-3 bytes past a word boundary. The previous sector leaves
   * non-0xFF bytes in the block buffer, so missing padding would write them. */
  static const uint32_t tails[] = {1, 2, 3, 5, 6, 7};
  for (size_t t = 0; t < sizeof(tails) / sizeof(tails[0]); t++) {
    uint32_t tail = tails[t];
    fake_flash *flash = new_flash();
    mg2x_writer *writer = malloc(sizeof(*writer));
    CHECK(writer != NULL);
    uint32_t base = 2u * MG2X_SECTOR_BYTES;
    uint32_t total = MG2X_SECTOR_BYTES + tail;
    CHECK(mg2x_writer_init(writer, ops_for(flash), base, 8u * MG2X_SECTOR_BYTES) ==
          MG2X_WRITER_OK);
    CHECK(mg2x_writer_push(writer, s_image, MG2X_SECTOR_BYTES) == MG2X_WRITER_OK);
    CHECK(mg2x_writer_push(writer, &s_image[5000], tail) == MG2X_WRITER_OK);
    CHECK(mg2x_writer_flush(writer) == MG2X_WRITER_OK);
    CHECK(memcmp(&flash->bytes[base], s_image, MG2X_SECTOR_BYTES) == 0);
    CHECK(memcmp(&flash->bytes[base + MG2X_SECTOR_BYTES], &s_image[5000], tail) == 0);
    uint32_t padded = (tail + 3u) & ~3u;
    for (uint32_t i = tail; i < padded; i++) {
      CHECK(flash->bytes[base + MG2X_SECTOR_BYTES + i] == 0xFF);
    }
    CHECK(flash->bytes[base + total + (padded - tail)] == 0xFF);
    CHECK(mg2x_writer_end_address(writer) == base + 2u * MG2X_SECTOR_BYTES);
    free(writer);
    free(flash);
  }
}

static void test_writer_never_crosses_limit(void) {
  fake_flash *flash = new_flash();
  mg2x_writer *writer = malloc(sizeof(*writer));
  CHECK(writer != NULL);
  CHECK(mg2x_writer_init(writer, ops_for(flash), 0, 2u * MG2X_SECTOR_BYTES) == MG2X_WRITER_OK);
  CHECK(mg2x_writer_push(writer, s_image, 2u * MG2X_SECTOR_BYTES) == MG2X_WRITER_OK);
  CHECK(mg2x_writer_push(writer, s_image, 1) == MG2X_WRITER_OK);
  CHECK(mg2x_writer_flush(writer) == MG2X_WRITER_ERR_LIMIT);
  free(writer);
  free(flash);
}

static void test_writer_reports_flash_errors(void) {
  fake_flash *flash = new_flash();
  mg2x_writer *writer = malloc(sizeof(*writer));
  CHECK(writer != NULL);
  flash->fail_erase_at_call = 1;
  CHECK(mg2x_writer_init(writer, ops_for(flash), 0, 8u * MG2X_SECTOR_BYTES) == MG2X_WRITER_OK);
  CHECK(mg2x_writer_push(writer, s_image, MG2X_SECTOR_BYTES) == MG2X_WRITER_ERR_FLASH);

  flash->fail_erase_at_call = 0;
  flash->erase_calls = 0;
  flash->fail_write_at_call = 1;
  CHECK(mg2x_writer_init(writer, ops_for(flash), 0, 8u * MG2X_SECTOR_BYTES) == MG2X_WRITER_OK);
  CHECK(mg2x_writer_push(writer, s_image, MG2X_SECTOR_BYTES) == MG2X_WRITER_ERR_FLASH);
  free(writer);
  free(flash);
}

static void test_writer_result_strings(void) {
  CHECK(strcmp(mg2x_writer_result_str(MG2X_WRITER_OK), "ok") == 0);
  CHECK(strcmp(mg2x_writer_result_str(MG2X_WRITER_ERR_ALIGNMENT), "alignment") == 0);
  CHECK(strcmp(mg2x_writer_result_str(MG2X_WRITER_ERR_LIMIT), "limit") == 0);
  CHECK(strcmp(mg2x_writer_result_str(MG2X_WRITER_ERR_FLASH), "flash") == 0);
  CHECK(strcmp(mg2x_writer_result_str((mg2x_writer_result)99), "unknown") == 0);
}

/* ---- installer ---------------------------------------------------------- */

#define S MG2X_SECTOR_BYTES
#define INSTALL_IMAGE_BYTES 40000u
#define INSTALL_STAGING (16u * S)
#define INSTALL_LIMIT (48u * S)

static _Alignas(4) uint8_t s_install_buffer[MG2X_SECTOR_BYTES];

static fake_flash *prepared_flash(uint32_t boot_config) {
  fake_flash *flash = new_flash();
  memset(flash->bytes, 0xFF, sizeof(flash->bytes));
  memset(&flash->bytes[0], 0xB0, S);           /* current bootloader */
  memset(&flash->bytes[boot_config], 0xC0, S); /* current boot config */
  for (uint32_t i = 0; i < INSTALL_IMAGE_BYTES; i++) {
    flash->bytes[INSTALL_STAGING + i] = (uint8_t)(i % 251u);
  }
  flash->bytes[INSTALL_STAGING] = MG2X_IMAGE_MAGIC;
  return flash;
}

static int boot_sectors_unchanged(const fake_flash *flash, uint32_t boot_config) {
  for (uint32_t i = 0; i < S; i++) {
    if (flash->bytes[i] != 0xB0 || flash->bytes[boot_config + i] != 0xC0) {
      return 0;
    }
  }
  return 1;
}

static mg2x_install_layout layout_for(uint32_t boot_config) {
  mg2x_install_layout layout = {INSTALL_STAGING, INSTALL_IMAGE_BYTES, boot_config, INSTALL_LIMIT};
  return layout;
}

static void test_install_moves_the_image_into_place(void) {
  static const uint32_t configs[] = {0x1000u, 0x7000u};
  for (size_t c = 0; c < 2u; c++) {
    fake_flash *flash = prepared_flash(configs[c]);
    uint8_t *expected = malloc(INSTALL_IMAGE_BYTES);
    CHECK(expected != NULL);
    memcpy(expected, &flash->bytes[INSTALL_STAGING], INSTALL_IMAGE_BYTES);
    mg2x_flash_ops ops = ops_for(flash);
    mg2x_install_layout layout = layout_for(configs[c]);
    CHECK(mg2x_install(&ops, &layout, s_install_buffer) == MG2X_INSTALL_OK);
    CHECK(memcmp(flash->bytes, expected, INSTALL_IMAGE_BYTES) == 0);
    free(expected);
    free(flash);
  }
}

static int count_ops_of_successful_install(uint32_t boot_config) {
  fake_flash *flash = prepared_flash(boot_config);
  mg2x_flash_ops ops = ops_for(flash);
  mg2x_install_layout layout = layout_for(boot_config);
  CHECK(mg2x_install(&ops, &layout, s_install_buffer) == MG2X_INSTALL_OK);
  int ops_total = flash->ops;
  free(flash);
  return ops_total;
}

/* Any single flash failure leaves the original boot sectors in place. */
static void test_install_single_failure_is_always_retryable(void) {
  static const uint32_t configs[] = {0x1000u, 0x7000u};
  for (size_t c = 0; c < 2u; c++) {
    int total = count_ops_of_successful_install(configs[c]);
    for (int k = 1; k <= total; k++) {
      fake_flash *flash = prepared_flash(configs[c]);
      flash->fail_op_at = k;
      mg2x_flash_ops ops = ops_for(flash);
      mg2x_install_layout layout = layout_for(configs[c]);
      mg2x_install_result result = mg2x_install(&ops, &layout, s_install_buffer);
      CHECK(result == MG2X_INSTALL_RETRYABLE);
      CHECK(boot_sectors_unchanged(flash, configs[c]));
      free(flash);
    }
  }
}

/* A permanently failing flash is only fatal once the boot sectors were touched. */
static void test_install_persistent_failure(void) {
  uint32_t config = 0x7000u;
  int total = count_ops_of_successful_install(config);
  int fatal = 0;
  int retryable = 0;
  for (int k = 1; k <= total; k++) {
    fake_flash *flash = prepared_flash(config);
    flash->fail_op_at = k;
    flash->fail_persistent = 1;
    mg2x_flash_ops ops = ops_for(flash);
    mg2x_install_layout layout = layout_for(config);
    mg2x_install_result result = mg2x_install(&ops, &layout, s_install_buffer);
    CHECK(result == MG2X_INSTALL_RETRYABLE || result == MG2X_INSTALL_FATAL);
    if (result == MG2X_INSTALL_RETRYABLE) {
      retryable++;
      CHECK(boot_sectors_unchanged(flash, config));
    } else {
      fatal++;
    }
    free(flash);
  }
  /* Only failures that start while the boot sectors are written are fatal. */
  CHECK(fatal > 0);
  CHECK(retryable > fatal);
}

/* Failing boot sector write plus failing restore of the bootloader is fatal. */
static void test_install_failed_restore_is_fatal(void) {
  uint32_t config = 0x1000u;
  int total = count_ops_of_successful_install(config);
  fake_flash *flash = prepared_flash(config);
  /* The last operation writes the new bootloader; the restore then copies the
   * boot config (read, erase, write) and fails reading the saved bootloader. */
  flash->fail_op_at = total;
  flash->fail_op_at_second = total + 4;
  mg2x_flash_ops ops = ops_for(flash);
  mg2x_install_layout layout = layout_for(config);
  CHECK(mg2x_install(&ops, &layout, s_install_buffer) == MG2X_INSTALL_FATAL);
  free(flash);
}

static void test_install_rejects_bad_layouts(void) {
  fake_flash *flash = prepared_flash(0x1000u);
  mg2x_flash_ops ops = ops_for(flash);
  const mg2x_install_layout bad[] = {
      {INSTALL_STAGING + 1u, INSTALL_IMAGE_BYTES, 0x1000u, INSTALL_LIMIT}, /* staging */
      {INSTALL_STAGING, INSTALL_IMAGE_BYTES, 0x1001u, INSTALL_LIMIT},      /* config */
      {INSTALL_STAGING, INSTALL_IMAGE_BYTES, 0x1000u, INSTALL_LIMIT + 1u}, /* limit */
      {INSTALL_STAGING, INSTALL_IMAGE_BYTES, 0u, INSTALL_LIMIT},           /* config < S */
      {0x7000u, INSTALL_IMAGE_BYTES, 0x7000u, INSTALL_LIMIT},              /* staging <= config */
      {INSTALL_STAGING, 0x1000u + 2u * S - 1u, 0x1000u, INSTALL_LIMIT},    /* image too small */
      {INSTALL_STAGING, INSTALL_IMAGE_BYTES, 0x1000u, 20u * S},            /* scratch > limit */
      {INSTALL_STAGING, INSTALL_IMAGE_BYTES, 0x1000u, INSTALL_STAGING},    /* limit <= staging */
      {INSTALL_STAGING, 32u * S + 1u, 0x1000u, INSTALL_LIMIT},             /* image > room */
  };
  for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); i++) {
    CHECK(mg2x_install(&ops, &bad[i], s_install_buffer) == MG2X_INSTALL_BAD_LAYOUT);
  }
  CHECK(flash->ops == 0);
  CHECK(mg2x_flash_copy(&ops, 0, S, 0, s_install_buffer));
  CHECK(flash->ops == 0);
  free(flash);
}

static void test_install_result_strings(void) {
  CHECK(strcmp(mg2x_install_result_str(MG2X_INSTALL_OK), "ok") == 0);
  CHECK(strcmp(mg2x_install_result_str(MG2X_INSTALL_RETRYABLE), "install_retryable") == 0);
  CHECK(strcmp(mg2x_install_result_str(MG2X_INSTALL_FATAL), "install_fatal") == 0);
  CHECK(strcmp(mg2x_install_result_str(MG2X_INSTALL_BAD_LAYOUT), "install_layout") == 0);
  CHECK(strcmp(mg2x_install_result_str((mg2x_install_result)99), "unknown") == 0);
}

int main(void) {
  make_image();
  test_sha256_vectors();
  test_sha256_incremental_matches_one_shot();
  test_job_begin_validation();
  test_job_accepts_uppercase_hash_and_streams();
  test_job_rejects_bad_streams();
  test_job_result_strings();
  test_writer_init_alignment();
  test_writer_handles_any_chunk_size();
  test_writer_pads_unaligned_tails();
  test_writer_never_crosses_limit();
  test_writer_reports_flash_errors();
  test_writer_result_strings();
  test_install_moves_the_image_into_place();
  test_install_single_failure_is_always_retryable();
  test_install_persistent_failure();
  test_install_failed_restore_is_fatal();
  test_install_rejects_bad_layouts();
  test_install_result_strings();
  if (s_failures != 0) {
    fprintf(stderr, "%d check(s) failed\n", s_failures);
    return 1;
  }
  printf("all migration firmware host tests passed\n");
  return 0;
}
