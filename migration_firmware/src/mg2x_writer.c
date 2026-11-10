/*
 * SPDX-License-Identifier: GPL-3.0-only
 */
#include "mg2x_writer.h"

#include <string.h>

mg2x_writer_result mg2x_writer_init(mg2x_writer *writer, mg2x_flash_ops ops, uint32_t base_address,
                                    uint32_t limit_address) {
  if (base_address % MG2X_SECTOR_BYTES != 0u || limit_address % MG2X_SECTOR_BYTES != 0u ||
      limit_address <= base_address) {
    return MG2X_WRITER_ERR_ALIGNMENT;
  }
  memset(writer, 0, sizeof(*writer));
  writer->ops = ops;
  writer->base_address = base_address;
  writer->limit_address = limit_address;
  return MG2X_WRITER_OK;
}

static mg2x_writer_result write_block(mg2x_writer *writer) {
  uint32_t address = writer->base_address + writer->flushed_bytes;
  if (address + MG2X_SECTOR_BYTES > writer->limit_address) {
    return MG2X_WRITER_ERR_LIMIT;
  }
  if (writer->ops.erase_sector(writer->ops.ctx, address / MG2X_SECTOR_BYTES) != 0) {
    return MG2X_WRITER_ERR_FLASH;
  }
  if (writer->ops.write(writer->ops.ctx, address, writer->block, writer->fill_bytes) != 0) {
    return MG2X_WRITER_ERR_FLASH;
  }
  writer->flushed_bytes += MG2X_SECTOR_BYTES;
  writer->fill_bytes = 0;
  return MG2X_WRITER_OK;
}

mg2x_writer_result mg2x_writer_push(mg2x_writer *writer, const uint8_t *data, size_t len) {
  while (len > 0u) {
    size_t space = MG2X_SECTOR_BYTES - writer->fill_bytes;
    size_t take = len < space ? len : space;
    memcpy(&writer->block[writer->fill_bytes], data, take);
    writer->fill_bytes += (uint32_t)take;
    data += take;
    len -= take;
    if (writer->fill_bytes == MG2X_SECTOR_BYTES) {
      mg2x_writer_result result = write_block(writer);
      if (result != MG2X_WRITER_OK) {
        return result;
      }
    }
  }
  return MG2X_WRITER_OK;
}

mg2x_writer_result mg2x_writer_flush(mg2x_writer *writer) {
  if (writer->fill_bytes == 0u) {
    return MG2X_WRITER_OK;
  }
  /* The ESP8266 writes in 4-byte words; pad the tail with erased-flash value. */
  uint32_t padded = (writer->fill_bytes + 3u) & ~3u;
  memset(&writer->block[writer->fill_bytes], 0xFF, padded - writer->fill_bytes);
  writer->fill_bytes = padded;
  return write_block(writer);
}

uint32_t mg2x_writer_end_address(const mg2x_writer *writer) {
  return writer->base_address + writer->flushed_bytes;
}

const char *mg2x_writer_result_str(mg2x_writer_result result) {
  switch (result) {
  case MG2X_WRITER_OK:
    return "ok";
  case MG2X_WRITER_ERR_ALIGNMENT:
    return "alignment";
  case MG2X_WRITER_ERR_LIMIT:
    return "limit";
  case MG2X_WRITER_ERR_FLASH:
    return "flash";
  }
  return "unknown";
}
