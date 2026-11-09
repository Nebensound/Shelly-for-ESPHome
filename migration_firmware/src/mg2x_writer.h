/*
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * Buffers an arbitrary stream of bytes into flash-sector sized blocks and
 * writes them to a staging area. The flash access is injected so the logic can
 * be unit tested on the host with an in-memory flash.
 */
#ifndef MG2X_WRITER_H
#define MG2X_WRITER_H

#include <stddef.h>
#include <stdint.h>

#include "mg2x_flash.h"

typedef enum {
  MG2X_WRITER_OK = 0,
  MG2X_WRITER_ERR_ALIGNMENT,
  MG2X_WRITER_ERR_LIMIT,
  MG2X_WRITER_ERR_FLASH
} mg2x_writer_result;

typedef struct {
  mg2x_flash_ops ops;
  uint32_t base_address;
  /* First address that must never be written (e.g. the running program). */
  uint32_t limit_address;
  uint32_t flushed_bytes;
  uint32_t fill_bytes;
  /* spi_flash_write() requires a 4-byte aligned source buffer. */
  _Alignas(4) uint8_t block[MG2X_SECTOR_BYTES];
} mg2x_writer;

mg2x_writer_result mg2x_writer_init(mg2x_writer *writer, mg2x_flash_ops ops, uint32_t base_address,
                                    uint32_t limit_address);

mg2x_writer_result mg2x_writer_push(mg2x_writer *writer, const uint8_t *data, size_t len);

/* Writes a partially filled last block. Safe to call when nothing is pending. */
mg2x_writer_result mg2x_writer_flush(mg2x_writer *writer);

/* Address of the first sector after everything written so far. */
uint32_t mg2x_writer_end_address(const mg2x_writer *writer);

const char *mg2x_writer_result_str(mg2x_writer_result result);

#endif /* MG2X_WRITER_H */
