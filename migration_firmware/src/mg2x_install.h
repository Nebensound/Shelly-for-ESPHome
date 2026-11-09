/*
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * Moves a verified image from the staging area to address 0x0.
 *
 * Order of operations:
 *   1. keep the new bootloader/boot config and the current bootloader/boot
 *      config in four scratch sectors behind the staged image,
 *   2. move the image body (everything except sector 0 and the boot config
 *      sector) into place,
 *   3. write the new boot config, then the new bootloader.
 *
 * Steps 1 and 2 never touch sector 0 or the boot config sector, so a failure
 * there is retryable: the running firmware still boots. If step 3 fails, the
 * saved original sectors are written back; only if that also fails is the
 * result fatal.
 */
#ifndef MG2X_INSTALL_H
#define MG2X_INSTALL_H

#include <stdbool.h>
#include <stdint.h>

#include "mg2x_flash.h"

/* Sectors needed behind the staged image. */
#define MG2X_INSTALL_SCRATCH_SECTORS 4u

typedef enum {
  MG2X_INSTALL_OK = 0,
  /* Nothing the bootloader needs was changed (or it was restored). */
  MG2X_INSTALL_RETRYABLE,
  /* Boot sectors could neither be written nor restored: needs a cable. */
  MG2X_INSTALL_FATAL,
  /* Layout parameters are inconsistent; nothing was written. */
  MG2X_INSTALL_BAD_LAYOUT
} mg2x_install_result;

typedef struct {
  uint32_t staging_address;     /* sector aligned, above the boot config */
  uint32_t image_bytes;         /* verified image size */
  uint32_t boot_config_address; /* sector aligned, >= one sector */
  uint32_t limit_address;       /* first address that must not be written */
} mg2x_install_layout;

/*
 * Copies len_bytes (rounded up to whole words) sector by sector using a
 * sector-sized, 4-byte aligned buffer. Safe for overlapping regions when
 * copying downwards by at least one sector.
 */
bool mg2x_flash_copy(const mg2x_flash_ops *ops, uint32_t src, uint32_t dest, uint32_t len_bytes,
                     uint8_t *buffer);

mg2x_install_result mg2x_install(const mg2x_flash_ops *ops, const mg2x_install_layout *layout,
                                 uint8_t *buffer);

const char *mg2x_install_result_str(mg2x_install_result result);

#endif /* MG2X_INSTALL_H */
