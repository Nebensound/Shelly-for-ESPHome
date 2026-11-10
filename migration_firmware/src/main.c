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
 *  - progress and errors are reported via MG2X.Status,
 *  - the original boot sectors are kept and written back if installing the
 *    new ones fails (see mg2x_install.h).
 *
 * Flash layout during the job (rboot, two ROM slots):
 *   0x0                bootloader            <- overwritten last
 *   BOOT_CONFIG_ADDR   rboot config          <- overwritten second to last
 *   roms[0]            staging area          <- download target
 *   roms[1]            this firmware (running, never written)
 */
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>

#include "mgos.h"
#include "mgos_rpc.h"
#include "frozen.h"
#include "spi_flash.h"
#include "esp_rboot.h"

#include "mg2x_install.h"
#include "mg2x_job.h"
#include "mg2x_writer.h"

#define MG2X_PROTOCOL_VERSION 2
#define MG2X_STAGING_ROM 0
#define MG2X_RUNNING_ROM 1
/* Scratch sectors of the installer plus one sector of margin. */
#define MG2X_RESERVED_SECTORS (MG2X_INSTALL_SCRATCH_SECTORS + 1u)
#define MG2X_RELOCATION_RETRY_MS 30000
#define MG2X_MAX_REDIRECTS 3
#define MG2X_STALL_TIMEOUT_MICROS (60 * 1000 * 1000)
#define MG2X_WATCHDOG_INTERVAL_MS 5000

/* The CLI checks the migration package for this exact marker. */
static const char s_marker[] = "shelly2esphome-mg2x-protocol=2";

typedef enum {
  MG2X_STATE_IDLE,
  MG2X_STATE_DOWNLOADING,
  MG2X_STATE_INSTALLING,
  MG2X_STATE_FAILED,
  MG2X_STATE_RELOCATION_FAILED
} mg2x_state;

static rboot_config *s_rboot_cfg;
static mg2x_state s_state = MG2X_STATE_IDLE;
static mg2x_job s_job;
static mg2x_writer s_writer;
static char s_error[32] = "";
static struct mg_connection *s_conn = NULL;
/* Connection generation, passed to mg_connect_http() as user data. It is a
 * uintptr_t so the round trip through void * is lossless; http_cb() only
 * compares it and never dereferences it. */
static uintptr_t s_generation = 0;
static int s_redirects = 0;
static int64_t s_last_progress_micros = 0;
/* Set once an install left the boot sectors unusable; cleared by success. */
static bool s_boot_damaged = false;

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
  case MG2X_STATE_RELOCATION_FAILED:
    return "relocation_failed";
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

/* Largest image that leaves room for the installer's scratch sectors. */
static uint32_t max_image_bytes(void) {
  if (s_rboot_cfg->current_rom != MG2X_RUNNING_ROM) {
    return 0u; /* staging is only possible once we run from slot 1 */
  }
  uint32_t area = running_address() - staging_address();
  uint32_t reserved = MG2X_RESERVED_SECTORS * MG2X_SECTOR_BYTES;
  return area > reserved ? area - reserved : 0u;
}

/* The install step moves the region behind the boot config separately. */
static uint32_t min_image_bytes(void) {
  return (uint32_t)BOOT_CONFIG_ADDR + 2u * MG2X_SECTOR_BYTES;
}

/* spi_flash_read/write need 4-byte aligned buffers, addresses and lengths.
 * Enforce that here so a future caller cannot cause an unaligned access. */
static bool flash_aligned(uint32_t address, const uint8_t *data, uint32_t len_bytes) {
  return ((address | len_bytes) & 3u) == 0u && ((uintptr_t)data & (uintptr_t)3u) == 0u;
}

static int flash_read(void *ctx, uint32_t address, uint8_t *data, uint32_t len_bytes) {
  (void)ctx;
  if (!flash_aligned(address, data, len_bytes)) {
    return 1;
  }
  return spi_flash_read(address, (uint32 *)(uintptr_t)data, len_bytes) == SPI_FLASH_RESULT_OK ? 0
                                                                                              : 1;
}

static int flash_erase(void *ctx, uint32_t sector_index) {
  (void)ctx;
  if (sector_index > UINT16_MAX) {
    return 1; /* the SDK takes a 16-bit index; never let it wrap */
  }
  return spi_flash_erase_sector((uint16)sector_index) == SPI_FLASH_RESULT_OK ? 0 : 1;
}

static int flash_write(void *ctx, uint32_t address, const uint8_t *data, uint32_t len_bytes) {
  (void)ctx;
  if (!flash_aligned(address, data, len_bytes)) {
    return 1;
  }
  return spi_flash_write(address, (uint32 *)(uintptr_t)data, len_bytes) == SPI_FLASH_RESULT_OK ? 0
                                                                                               : 1;
}

static const mg2x_flash_ops s_flash_ops = {flash_read, flash_erase, flash_write, NULL};

/* Moves the verified image from the staging area to 0x0 and restarts. */
static void install(void) {
  s_state = MG2X_STATE_INSTALLING;
  mg2x_install_layout layout = {
      .staging_address = staging_address(),
      .image_bytes = s_job.received_bytes,
      .boot_config_address = (uint32_t)BOOT_CONFIG_ADDR,
      .limit_address = running_address(),
  };
  LOG(LL_INFO, ("image verified (%lu bytes), installing", (unsigned long)layout.image_bytes));
  /* writer.block is free again after the final flush. */
  mg2x_install_result result =
      mg2x_install_track(mg2x_install(&s_flash_ops, &layout, s_writer.block), &s_boot_damaged);
  if (result != MG2X_INSTALL_OK) {
    fail(mg2x_install_result_str(result));
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
  int parsed = json_scanf(args.p, args.len, ri->args_fmt, &url, &size_bytes, &sha256);

  if (parsed != 3 || url == NULL || sha256 == NULL) {
    mg_rpc_send_errorf(ri, 400, "bad_request");
  } else if (s_state == MG2X_STATE_DOWNLOADING || s_state == MG2X_STATE_INSTALLING) {
    mg_rpc_send_errorf(ri, 409, "busy");
#if ULONG_MAX > UINT32_MAX
  } else if (size_bytes > UINT32_MAX) {
    /* Never let a truncating cast turn an oversized request into a valid one. */
    mg_rpc_send_errorf(ri, 400, "%s", mg2x_job_result_str(MG2X_JOB_ERR_SIZE_TOO_LARGE));
#endif
  } else {
    mg2x_job_result job_result = mg2x_job_begin(&s_job, url, (uint32_t)size_bytes,
                                                min_image_bytes(), max_image_bytes(), sha256);
    mg2x_writer_result writer_result = MG2X_WRITER_OK;
    if (job_result == MG2X_JOB_OK) {
      writer_result =
          mg2x_writer_init(&s_writer, s_flash_ops, staging_address(), running_address());
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

static void restart_cb(void *arg) {
  (void)arg;
  mgos_system_restart();
}

enum mgos_app_init_result mgos_app_init(void) {
  s_rboot_cfg = get_rboot_config();
  LOG(LL_INFO, ("%s", s_marker));

  if (s_rboot_cfg->current_rom == MG2X_STAGING_ROM) {
    /* Move ourselves into slot 1 so slot 0 becomes the staging area. */
    LOG(LL_INFO, ("booted from slot 0, moving to slot 1"));
    bool ok =
        mg2x_flash_copy(&s_flash_ops, s_rboot_cfg->roms[0], s_rboot_cfg->roms[1],
                        s_rboot_cfg->roms_sizes[0], s_writer.block) &&
        mg2x_flash_copy(&s_flash_ops, s_rboot_cfg->fs_addresses[0], s_rboot_cfg->fs_addresses[1],
                        s_rboot_cfg->fs_sizes[0], s_writer.block);
    if (ok) {
      s_rboot_cfg->current_rom = MG2X_RUNNING_ROM;
      s_rboot_cfg->previous_rom = MG2X_STAGING_ROM;
      s_rboot_cfg->fw_updated = 1;
      s_rboot_cfg->is_first_boot = 1;
      rboot_set_config(s_rboot_cfg);
      mgos_system_restart_after(200);
    } else {
      /* Still booting from slot 0: report it and retry after a restart. Image
       * staging stays disabled (no MG2X.Flash) until we run from slot 1. */
      s_state = MG2X_STATE_RELOCATION_FAILED;
      snprintf(s_error, sizeof(s_error), "%s", "relocation_copy");
      LOG(LL_ERROR, ("moving to slot 1 failed, retrying after restart"));
      mg_rpc_add_handler(mgos_rpc_get_global(), "MG2X.Status", "", rpc_status, NULL);
      mgos_set_timer(MG2X_RELOCATION_RETRY_MS, 0, restart_cb, NULL);
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
