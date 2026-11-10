/*
 * SPDX-License-Identifier: GPL-3.0-only
 */
#include "mg2x_install.h"

static uint32_t round_up_sector(uint32_t bytes) {
  return (bytes + MG2X_SECTOR_BYTES - 1u) / MG2X_SECTOR_BYTES * MG2X_SECTOR_BYTES;
}

bool mg2x_flash_copy(const mg2x_flash_ops *ops, uint32_t src, uint32_t dest, uint32_t len_bytes,
                     uint8_t *buffer) {
  uint32_t total = (len_bytes + 3u) & ~3u;
  for (uint32_t offset = 0; offset < total; offset += MG2X_SECTOR_BYTES) {
    uint32_t chunk = total - offset < MG2X_SECTOR_BYTES ? total - offset : MG2X_SECTOR_BYTES;
    if (ops->read(ops->ctx, src + offset, buffer, chunk) != 0 ||
        ops->erase_sector(ops->ctx, (dest + offset) / MG2X_SECTOR_BYTES) != 0 ||
        ops->write(ops->ctx, dest + offset, buffer, chunk) != 0) {
      return false;
    }
  }
  return true;
}

static bool layout_is_valid(const mg2x_install_layout *l) {
  uint32_t s = MG2X_SECTOR_BYTES;
  if (l->staging_address % s != 0u || l->boot_config_address % s != 0u ||
      l->limit_address % s != 0u || l->boot_config_address < s) {
    return false;
  }
  /* The image body is moved downwards; the source must lie above the target. */
  if (l->staging_address <= l->boot_config_address) {
    return false;
  }
  /* The image has to cover the boot config sector plus at least one more. */
  if (l->image_bytes < l->boot_config_address + 2u * s) {
    return false;
  }
  if (l->limit_address <= l->staging_address) {
    return false;
  }
  uint32_t room = l->limit_address - l->staging_address;
  if (l->image_bytes > room) {
    return false;
  }
  uint64_t needed = (uint64_t)round_up_sector(l->image_bytes) + MG2X_INSTALL_SCRATCH_SECTORS * s;
  return needed <= room;
}

static mg2x_install_result restore(const mg2x_flash_ops *ops, const mg2x_install_layout *l,
                                   uint32_t old_boot, uint32_t old_config, uint8_t *buffer) {
  uint32_t s = MG2X_SECTOR_BYTES;
  bool ok = mg2x_flash_copy(ops, old_config, l->boot_config_address, s, buffer) &&
            mg2x_flash_copy(ops, old_boot, 0, s, buffer);
  return ok ? MG2X_INSTALL_RETRYABLE : MG2X_INSTALL_FATAL;
}

mg2x_install_result mg2x_install(const mg2x_flash_ops *ops, const mg2x_install_layout *l,
                                 uint8_t *buffer) {
  if (!layout_is_valid(l)) {
    return MG2X_INSTALL_BAD_LAYOUT;
  }
  uint32_t s = MG2X_SECTOR_BYTES;
  uint32_t staging = l->staging_address;
  uint32_t config = l->boot_config_address;
  uint32_t new_boot = staging + round_up_sector(l->image_bytes);
  uint32_t new_config = new_boot + s;
  uint32_t old_boot = new_config + s;
  uint32_t old_config = old_boot + s;

  /* 1. Scratch copies; sector 0 and the boot config are only read. */
  if (!mg2x_flash_copy(ops, staging, new_boot, s, buffer) ||
      !mg2x_flash_copy(ops, staging + config, new_config, s, buffer) ||
      !mg2x_flash_copy(ops, 0, old_boot, s, buffer) ||
      !mg2x_flash_copy(ops, config, old_config, s, buffer)) {
    return MG2X_INSTALL_RETRYABLE;
  }

  /* 2. Image body around the two boot sectors. */
  if (!mg2x_flash_copy(ops, staging + s, s, config - s, buffer) ||
      !mg2x_flash_copy(ops, staging + config + s, config + s, l->image_bytes - config - s,
                       buffer)) {
    return MG2X_INSTALL_RETRYABLE;
  }

  /* 3. Boot config, then bootloader; restore the originals on failure. Sector 0
   * is only rewritten during a restore if it was touched before. */
  if (!mg2x_flash_copy(ops, new_config, config, s, buffer)) {
    return mg2x_flash_copy(ops, old_config, config, s, buffer) ? MG2X_INSTALL_RETRYABLE
                                                               : MG2X_INSTALL_FATAL;
  }
  if (!mg2x_flash_copy(ops, new_boot, 0, s, buffer)) {
    return restore(ops, l, old_boot, old_config, buffer);
  }
  return MG2X_INSTALL_OK;
}

mg2x_install_result mg2x_install_track(mg2x_install_result result, bool *boot_damaged) {
  if (result == MG2X_INSTALL_OK) {
    *boot_damaged = false;
    return result;
  }
  if (result == MG2X_INSTALL_FATAL) {
    *boot_damaged = true;
  }
  return *boot_damaged ? MG2X_INSTALL_FATAL : result;
}

const char *mg2x_install_result_str(mg2x_install_result result) {
  switch (result) {
  case MG2X_INSTALL_OK:
    return "ok";
  case MG2X_INSTALL_RETRYABLE:
    return "install_retryable";
  case MG2X_INSTALL_FATAL:
    return "install_fatal";
  case MG2X_INSTALL_BAD_LAYOUT:
    return "install_layout";
  }
  return "unknown";
}
