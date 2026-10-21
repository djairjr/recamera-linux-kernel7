/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * recamera-cam -- the reCamera camera bring-up behind a small C ABI:
 * SYS/VB, the OV5647 (sensor object, MIPI, VI device/pipe/channel, ISP with
 * the 3A libraries and the tuning bin) and the cleanup of what a killed
 * process left in the kernel. Used by recamera-vicap and, through Rust FFI
 * (app/sophgo-sys), by recamerad; the frame path (VPSS, VENC, TPU) is called
 * directly by each program.
 *
 * Teardown order: VENC channels, VPSS groups, rc_cam_close(), rc_sys_close().
 * Every function returns 0 or the MPI error code.
 */
#ifndef RECAMERA_CAM_H
#define RECAMERA_CAM_H

#ifdef __cplusplus
extern "C" {
#endif

/* OV5647 modes (sophgo-mpi patch 0003) */
enum rc_sensor {
	RC_SENSOR_1280X960 = 0,	/* 2x2 binned, ~full field of view (default) */
	RC_SENSOR_1920X1080 = 1,	/* centre crop */
	RC_SENSOR_640X480 = 2,	/* binned + skipped */
};

struct rc_pool {
	unsigned int blk_size;
	unsigned int blk_cnt;
};

/* Output size of a sensor mode and the size of one VI frame block (the VI
 * pool of the offline mode). */
int rc_cam_size(int sensor, unsigned int *width, unsigned int *height,
		unsigned int *vi_blk_size);

/* Bytes of one frame (COMMON_GetPicBufferSize: 8 bit, uncompressed, default
 * alignment) -- the block size of a VB pool. pixfmt: PIXEL_FORMAT_E. */
unsigned int rc_buffer_size(unsigned int width, unsigned int height, int pixfmt);

/* Undo what a process killed without its teardown left in the kernel (VENC
 * channels, VPSS groups, VI pipe/device, VB pools: ION held until a reboot
 * and "exists" errors on the next start). Safe when nothing is left. Call
 * before rc_sys_open(). Returns the number of objects cleaned, or -pid when
 * another live process (pid) uses the MPI: then nothing is touched. */
int rc_reclaim(void);

/* SYS + VB with the given pools (cached, smallest block first: the VB takes
 * the first pool that fits). online: VI -> VPSS online (VPSS device 1 reads
 * the ISP output, no VI frame pool; the VPSS group then uses u8VpssDev 1 and
 * no bind). */
int rc_sys_open(const struct rc_pool *pools, unsigned int npools, int online);
void rc_sys_close(void);

/* Sensor, MIPI, VI device, pipe 0, ISP (3A thread, tuning bin), VI channel 0. */
int rc_cam_open(int sensor);
void rc_cam_close(void);

/* Picture adjustments of the running camera, each 0..100 (50: as the
 * tuning bin leaves it). brightness/contrast/saturation/hue: the ISP colour
 * space conversion; sharpness scales the tuned sharpen gain of every ISO
 * level (0 off, 100 twice). */
int rc_isp_image(unsigned int brightness, unsigned int contrast, unsigned int saturation,
		 unsigned int hue, unsigned int sharpness);

/* Mirror (left-right) and flip (upside down) of the camera picture. */
int rc_cam_flip(int mirror, int flip);

/* Hardware encoder channel (cvi_vc_driver; MaxVencChnNum=3), fed with VPSS
 * frames (NV21) of width x height. Close every channel before
 * rc_sys_close(). */
enum rc_codec {
	RC_CODEC_JPEG = 0,	/* param: quality 1..99 */
	RC_CODEC_H264 = 1,	/* Baseline (CAVLC), CBR; param: kbit/s; gop: frames */
};

int rc_venc_open(int chn, int codec, unsigned int width, unsigned int height,
		 unsigned int param, unsigned int gop);
void rc_venc_close(int chn);

/* H.264: make the next frame an IDR (a new viewer starts there). */
int rc_venc_request_idr(int chn);

/* Encode one frame (a VIDEO_FRAME_INFO_S of the MPI) and copy the result
 * (all packs, for H.264 an Annex B access unit) into buf. *len gets the
 * size; if it is larger than cap nothing is copied and the call fails:
 * call again with a buffer of *len bytes (the frame is lost). */
int rc_venc_encode(int chn, const void *frame, int timeout_ms, unsigned char *buf,
		   unsigned int cap, unsigned int *len);

#ifdef __cplusplus
}
#endif

#endif
