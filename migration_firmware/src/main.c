/*
 * SPDX-License-Identifier: GPL-3.0-only
 *
 * Intermediate firmware that replaces the Mongoose OS based Shelly Gen1 stock
 * firmware with a user-built ESPHome image, over the air.
 *
 * Based on mgos-to-tasmota by Mark Dornbach (GPL-3.0), changed so that:
 *  - nothing is downloaded automatically: the shelly2esphome CLI hands over
 *    URL, size and SHA-256 via the RPC method MG2X.Flash,
 *  - the image is verified (size, magic byte, SHA-256) before anything outside
 *    the staging area is touched; on any error the device stays in this
 *    firmware and accepts a new MG2X.Flash request,
 *  - progress and errors are reported via MG2X.Status.
 *
 * Flash layout during the job (rboot, two ROM slots):
 *   0x0                bootloader            <- overwritten last
 *   BOOT_CONFIG_ADDR   rboot config          <- overwritten second to last
 *   roms[0]            staging area          <- download target
 *   roms[1]            this firmware (running, never written)
 */
#include "mgos.h"
#include "mgos_rpc.h"
#include "frozen.h"
#include "spi_flash.h"
#include "esp_rboot.h"

#include "mg2x_job.h"
#include "mg2x_writer.h"

#define MG2X_PROTOCOL_VERSION 1
#define MG2X_STAGING_ROM 0
#define MG2X_RUNNING_ROM 1
#define MG2X_SCRATCH_SECTORS 3u
#define MG2X_MAX_REDIRECTS 3
#define MG2X_STALL_TIMEOUT_MICROS (60 * 1000 * 1000)
#define MG2X_WATCHDOG_INTERVAL_MS 5000

typedef enum {
  MG2X_STATE_IDLE,
  MG2X_STATE_DOWNLOADING,
  MG2X_STATE_INSTALLING,
  MG2X_STATE_FAILED
} mg2x_state;

static rboot_config *s_rboot_cfg;
static mg2x_state s_state = MG2X_STATE_IDLE;
static mg2x_job s_job;
static mg2x_writer s_writer;
static char s_error[32] = "";
static struct mg_connection *s_conn = NULL;
static uintptr_t s_generation = 0;
static int s_redirects = 0;
static int64_t s_last_progress_micros = 0;

static const char *state_str(mg2x_state state) {
  switch (state) {
  case MG2X_STATE_IDLE:
    return "idle";
  case MG2X_STATE_DOWNLOADING:
    return "downloading";
  case MG2X_STATE_INSTALLING:
    return "installing";
  case MG2X_STATE_FAILED:
    return "failed";
  }
  return "unknown";
}

static void fail(const char *reason) {
  s_state = MG2X_STATE_FAILED;
  snprintf(s_error, sizeof(s_error), "%s", reason);
  LOG(LL_ERROR, ("flash job failed: %s", reason));
}

static uint32_t staging_address(void) { return s_rboot_cfg->roms[MG2X_STAGING_ROM]; }

static uint32_t running_address(void) { return s_rboot_cfg->roms[s_rboot_cfg->current_rom]; }

/* Largest image that leaves room for the two scratch sectors used below. */
static uint32_t max_image_bytes(void) {
  uint32_t area = running_address() - staging_address();
  uint32_t reserved = MG2X_SCRATCH_SECTORS * MG2X_SECTOR_BYTES;
  return area > reserved ? area - reserved : 0u;
}

/* The install step moves the region behind the boot config separately. */
static uint32_t min_image_bytes(void) {
  return (uint32_t)BOOT_CONFIG_ADDR + 2u * MG2X_SECTOR_BYTES;
}

static int flash_erase(void *ctx, uint32_t sector_index) {
  (void)ctx;
  return spi_flash_erase_sector((uint16)sector_index) == SPI_FLASH_RESULT_OK ? 0 : 1;
}

static int flash_write(void *ctx, uint32_t address, const uint8_t *data, uint32_t len_bytes) {
  (void)ctx;
  /* writer.block is 4-byte aligned and len_bytes is a multiple of 4. */
  return spi_flash_write(address, (uint32 *)(uintptr_t)data, len_bytes) == SPI_FLASH_RESULT_OK ? 0
                                                                                               : 1;
}

/*
 * Copies `len_bytes` (rounded up to whole words) sector by sector. Source and
 * destination are sector aligned; copying downwards with src > dest is safe
 * for overlapping regions because each sector is read before it is erased.
 */
static bool block_copy(uint32_t src, uint32_t dest, uint32_t len_bytes) {
  uint8_t *buffer = s_writer.block; /* free again after the final flush */
  uint32_t total = (len_bytes + 3u) & ~3u;
  for (uint32_t offset = 0; offset < total; offset += MG2X_SECTOR_BYTES) {
    uint32_t chunk = total - offset < MG2X_SECTOR_BYTES ? total - offset : MG2X_SECTOR_BYTES;
    if (spi_flash_read(src + offset, (uint32 *)(uintptr_t)buffer, chunk) != SPI_FLASH_RESULT_OK ||
        spi_flash_erase_sector((uint16)((dest + offset) / MG2X_SECTOR_BYTES)) !=
            SPI_FLASH_RESULT_OK ||
        spi_flash_write(dest + offset, (uint32 *)(uintptr_t)buffer, chunk) != SPI_FLASH_RESULT_OK) {
      LOG(LL_ERROR, ("block copy 0x%lx -> 0x%lx failed at offset %lu", (unsigned long)src,
                     (unsigned long)dest, (unsigned long)offset));
      return false;
    }
  }
  return true;
}

/* Moves the verified image from the staging area to 0x0 and restarts. */
static void install(void) {
  s_state = MG2X_STATE_INSTALLING;
  uint32_t staging = staging_address();
  uint32_t received = s_job.received_bytes;
  uint32_t scratch_boot = mg2x_writer_end_address(&s_writer);
  uint32_t scratch_config = scratch_boot + MG2X_SECTOR_BYTES;
  uint32_t boot_config = (uint32_t)BOOT_CONFIG_ADDR;

  LOG(LL_INFO, ("image verified (%lu bytes), installing", (unsigned long)received));

  /* Keep bootloader and boot config of the new image aside ... */
  bool ok =
      block_copy(staging, scratch_boot, MG2X_SECTOR_BYTES) &&
      block_copy(staging + boot_config, scratch_config, MG2X_SECTOR_BYTES) &&
      /* ... move everything between them ... */
      block_copy(staging + MG2X_SECTOR_BYTES, MG2X_SECTOR_BYTES, boot_config - MG2X_SECTOR_BYTES) &&
      /* ... and everything after the boot config ... */
      block_copy(staging + boot_config + MG2X_SECTOR_BYTES, boot_config + MG2X_SECTOR_BYTES,
                 received - boot_config - MG2X_SECTOR_BYTES) &&
      /* ... then the boot config and the bootloader as the very last step. */
      block_copy(scratch_config, boot_config, MG2X_SECTOR_BYTES) &&
      block_copy(scratch_boot, 0, MG2X_SECTOR_BYTES);
  if (!ok) {
    fail("install_copy");
    return;
  }
  LOG(LL_INFO, ("install done, restarting into the new firmware"));
  mgos_system_restart_after(200);
}

static void finish_download(void) {
  mg2x_job_result job_result = mg2x_job_finish(&s_job);
  if (job_result != MG2X_JOB_OK) {
    fail(mg2x_job_result_str(job_result));
    return;
  }
  mg2x_writer_result writer_result = mg2x_writer_flush(&s_writer);
  if (writer_result != MG2X_WRITER_OK) {
    fail(mg2x_writer_result_str(writer_result));
    return;
  }
  install();
}

static void http_cb(struct mg_connection *c, int ev, void *ev_data, void *ud) {
  struct http_message *hm = (struct http_message *)ev_data;
  uintptr_t generation = (uintptr_t)ud;
  if (generation != s_generation) {
    /* Event of a connection that was replaced by a redirect. */
    if (ev == MG_EV_HTTP_CHUNK) {
      c->flags |= MG_F_DELETE_CHUNK;
    }
    return;
  }

  switch (ev) {
  case MG_EV_CONNECT:
    if (*(int *)ev_data != 0) {
      fail("connect_failed");
    }
    break;
  case MG_EV_HTTP_CHUNK: {
    c->flags |= MG_F_DELETE_CHUNK;
    if (hm->resp_code != 200 || s_state != MG2X_STATE_DOWNLOADING) {
      break;
    }
    const uint8_t *data = (const uint8_t *)hm->body.p;
    mg2x_job_result job_result = mg2x_job_feed(&s_job, data, hm->body.len);
    if (job_result != MG2X_JOB_OK) {
      fail(mg2x_job_result_str(job_result));
      c->flags |= MG_F_CLOSE_IMMEDIATELY;
      break;
    }
    mg2x_writer_result writer_result = mg2x_writer_push(&s_writer, data, hm->body.len);
    if (writer_result != MG2X_WRITER_OK) {
      fail(mg2x_writer_result_str(writer_result));
      c->flags |= MG_F_CLOSE_IMMEDIATELY;
      break;
    }
    s_last_progress_micros = mgos_uptime_micros();
    break;
  }
  case MG_EV_HTTP_REPLY:
    c->flags |= MG_F_CLOSE_IMMEDIATELY;
    if (s_state != MG2X_STATE_DOWNLOADING) {
      break;
    }
    if (hm->resp_code == 301 || hm->resp_code == 302 || hm->resp_code == 307 ||
        hm->resp_code == 308) {
      struct mg_str *location = mg_get_http_header(hm, "Location");
      if (location == NULL || s_redirects >= MG2X_MAX_REDIRECTS || location->len < 8 ||
          strncmp(location->p, "http://", 7) != 0) {
        fail("redirect");
        break;
      }
      char url[256];
      if (location->len >= sizeof(url)) {
        fail("redirect");
        break;
      }
      memcpy(url, location->p, location->len);
      url[location->len] = '\0';
      s_redirects++;
      s_generation++;
      s_conn = mg_connect_http(mgos_get_mgr(), http_cb, (void *)s_generation, url, NULL, NULL);
      if (s_conn == NULL) {
        fail("connect_failed");
      }
    } else if (hm->resp_code != 200) {
      char reason[16];
      snprintf(reason, sizeof(reason), "http_%d", hm->resp_code);
      fail(reason);
    }
    break;
  case MG_EV_CLOSE:
    s_conn = NULL;
    if (s_state == MG2X_STATE_DOWNLOADING) {
      /* Also reached when the transfer was cut off: finish reports it. */
      finish_download();
    }
    break;
  default:
    break;
  }
}

static void watchdog_cb(void *arg) {
  (void)arg;
  if (s_state == MG2X_STATE_DOWNLOADING &&
      mgos_uptime_micros() - s_last_progress_micros > MG2X_STALL_TIMEOUT_MICROS) {
    fail("timeout");
    if (s_conn != NULL) {
      s_conn->flags |= MG_F_CLOSE_IMMEDIATELY;
    }
  }
}

static void rpc_status(struct mg_rpc_request_info *ri, void *cb_arg, struct mg_rpc_frame_info *fi,
                       struct mg_str args) {
  mg_rpc_send_responsef(
      ri,
      "{protocol: %d, state: %Q, received: %lu, size: %lu, max_size: %lu, "
      "error: %Q}",
      MG2X_PROTOCOL_VERSION, state_str(s_state), (unsigned long)s_job.received_bytes,
      (unsigned long)s_job.expected_size_bytes, (unsigned long)max_image_bytes(), s_error);
  (void)cb_arg;
  (void)fi;
  (void)args;
}

static void rpc_flash(struct mg_rpc_request_info *ri, void *cb_arg, struct mg_rpc_frame_info *fi,
                      struct mg_str args) {
  char *url = NULL;
  char *sha256 = NULL;
  unsigned long size_bytes = 0;
  json_scanf(args.p, args.len, ri->args_fmt, &url, &size_bytes, &sha256);

  if (s_state == MG2X_STATE_DOWNLOADING || s_state == MG2X_STATE_INSTALLING) {
    mg_rpc_send_errorf(ri, 409, "busy");
  } else {
    mg2x_job_result job_result = mg2x_job_begin(&s_job, url, (uint32_t)size_bytes,
                                                min_image_bytes(), max_image_bytes(), sha256);
    mg2x_writer_result writer_result = MG2X_WRITER_OK;
    if (job_result == MG2X_JOB_OK) {
      mg2x_flash_ops ops = {flash_erase, flash_write, NULL};
      writer_result = mg2x_writer_init(&s_writer, ops, staging_address(), running_address());
    }
    if (job_result != MG2X_JOB_OK) {
      mg_rpc_send_errorf(ri, 400, "%s", mg2x_job_result_str(job_result));
    } else if (writer_result != MG2X_WRITER_OK) {
      mg_rpc_send_errorf(ri, 500, "%s", mg2x_writer_result_str(writer_result));
    } else {
      LOG(LL_INFO, ("flash job: %s (%lu bytes)", url, size_bytes));
      s_error[0] = '\0';
      s_redirects = 0;
      s_generation++;
      s_last_progress_micros = mgos_uptime_micros();
      s_state = MG2X_STATE_DOWNLOADING;
      s_conn = mg_connect_http(mgos_get_mgr(), http_cb, (void *)s_generation, url, NULL, NULL);
      if (s_conn == NULL) {
        fail("connect_failed");
      }
      mg_rpc_send_responsef(ri, "{started: %B}", s_conn != NULL);
    }
  }
  free(url);
  free(sha256);
  (void)cb_arg;
  (void)fi;
}

enum mgos_app_init_result mgos_app_init(void) {
  s_rboot_cfg = get_rboot_config();

  if (s_rboot_cfg->current_rom == MG2X_STAGING_ROM) {
    /* Move ourselves into slot 1 so slot 0 becomes the staging area. */
    LOG(LL_INFO, ("booted from slot 0, moving to slot 1"));
    bool ok = block_copy(s_rboot_cfg->roms[0], s_rboot_cfg->roms[1], s_rboot_cfg->roms_sizes[0]) &&
              block_copy(s_rboot_cfg->fs_addresses[0], s_rboot_cfg->fs_addresses[1],
                         s_rboot_cfg->fs_sizes[0]);
    if (ok) {
      s_rboot_cfg->current_rom = MG2X_RUNNING_ROM;
      s_rboot_cfg->previous_rom = MG2X_STAGING_ROM;
      s_rboot_cfg->fw_updated = 1;
      s_rboot_cfg->is_first_boot = 1;
      rboot_set_config(s_rboot_cfg);
      mgos_system_restart_after(200);
    }
    return MGOS_APP_INIT_SUCCESS;
  }

  LOG(LL_INFO,
      ("ready, waiting for MG2X.Flash (max image %lu bytes)", (unsigned long)max_image_bytes()));
  mg_rpc_add_handler(mgos_rpc_get_global(), "MG2X.Status", "", rpc_status, NULL);
  mg_rpc_add_handler(mgos_rpc_get_global(), "MG2X.Flash", "{url: %Q, size: %lu, sha256: %Q}",
                     rpc_flash, NULL);
  mgos_set_timer(MG2X_WATCHDOG_INTERVAL_MS, MGOS_TIMER_REPEAT, watchdog_cb, NULL);
  return MGOS_APP_INIT_SUCCESS;
}
