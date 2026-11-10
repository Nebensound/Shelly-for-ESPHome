/*
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * Injected flash access shared by the writer and the installer, so both can be
 * unit tested on the host with an in-memory flash.
 */
#ifndef MG2X_FLASH_H
#define MG2X_FLASH_H

#include <stdint.h>

#define MG2X_SECTOR_BYTES 4096u

typedef struct {
  /* All return 0 on success, like the ESP8266 SDK functions they wrap.
   * Buffers are 4-byte aligned and lengths are multiples of 4. */
  int (*read)(void *ctx, uint32_t address, uint8_t *data, uint32_t len_bytes);
  int (*erase_sector)(void *ctx, uint32_t sector_index);
  int (*write)(void *ctx, uint32_t address, const uint8_t *data, uint32_t len_bytes);
  void *ctx;
} mg2x_flash_ops;

#endif /* MG2X_FLASH_H */
