// SPDX-License-Identifier: GPL-2.0
/*
 * recamera-vicap -- F7 step 4c check: first frame from the reCamera OV5647 through the
 * Sophgo MPI (sys + vi + isp/3A) on Linux 7.2.
 *
 * The camera bring-up (SYS/VB, sensor, VI, ISP) and the cleanup of a killed
 * run are the recamera-cam library, shared with recamerad.
 *
 * Usage: recamera-vicap [frames_to_skip] [out.yuv] [vpss|jpeg [n]|h264 [n]|
 *                       yolo [n] [model] [class] [thr]|all [n] [model] [class] [thr]]
 *   defaults 30, /data/vicap.yuv. Output: NV21 frame (Y + interleaved VU).
 *   "vpss" (F7 step 5): VI bound to VPSS group 0 with two channels, saved as
 *   <out>.ch0.rgbp (640x640 RGB planar, letterboxed with grey 114: YOLO input)
 *   and <out>.ch1.nv21 (1280x720 NV21: stream).
 *   "jpeg" (F7 step 6-7): the VPSS pipeline, then n (default 30) ch1 frames
 *   sent to a hardware JPEG channel of the VENC; the last one is saved as
 *   <out>.jpg and the encode rate is printed.
 *   "h264" (F7 step 8): the same with a hardware H.264 channel (Baseline,
 *   CBR 2 Mbit/s, GOP 30); all n (default 150) frames go to <out>.h264
 *   (Annex B elementary stream, plays with ffplay/mpv/VLC).
 *   "yolo" (F7 step 9, built with sophgo-cviruntime): ch0 becomes 640x640
 *   RGB packed (the YOLO11 cvimodel input) and goes to the NPU without a
 *   copy, n (default 30) frames; detections of class (default 0, person;
 *   -1 any) above thr (default 0.5) are printed, the last ch0 frame is saved
 *   as <out>.rgb (raw RGB24) with its detections in <out>.det.
 *   Default model /data/models/yolo11n_detection_cv181x_int8.cvimodel.
 *   "all" (F7 step 10): the application's load, yolo on ch0 while a thread
 *   sends every ch1 frame to H.264 (<out>.h264) and JPEG; prints both rates.
 *   Environment: VICAP_ONLINE=0 runs VI -> VPSS offline (default online, no
 *   1080p VI pool); VICAP_VPSS_BLKS=n sets the blocks of each VPSS output
 *   pool (default 4); VICAP_AE_EVERY=n prints the ISP exposure every n
 *   encoded frames; VICAP_SENSOR=1080 or 640 selects the 1920x1080 centre
 *   crop or the 640x480 (binned + skipped) sensor mode instead of the
 *   default 1280x960 (2x2 binned, ~full field of view); with a 4:3 mode the
 *   stream channel takes the 16:9 centre band (1280x720 unscaled from
 *   1280x960).
 */
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <time.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "sample_comm.h"
#include "cvi_isp.h"
#include "cvi_ae.h"
#include "cvi_vpss.h"
#include "cvi_venc.h"
#include "recamera-cam.h"
#ifdef RECAMERA_NPU
#include "recamera-yolo.h"
#endif

static int g_use_vpss;
static PAYLOAD_TYPE_E g_enc; /* PT_JPEG / PT_H264 when encoding */
static int g_yolo;
static int g_all;	/* "all": YOLO + H.264 + JPEG at once (F7 step 10) */
/* Online by default (F7 step 10, the application's configuration): the ISP
 * output goes straight into the VPSS scaler (VI_OFFLINE_VPSS_ONLINE, VPSS
 * device 1 with ISP input), so no 1080p VI frame pool in DDR (9.1 MB of ION).
 * VICAP_ONLINE=0: offline (VI writes frames, VPSS reads them). */
static int g_online;
/* VPSS ch0: RGB planar (step 5) or packed, the YOLO11 cvimodel input */
#define CH0_FMT (g_yolo || g_all ? PIXEL_FORMAT_RGB_888 : PIXEL_FORMAT_RGB_888_PLANAR)

#define VPSS_GRP0	0
#define YOLO_W		640
#define YOLO_H		640
#define STREAM_W	1280
#define STREAM_H	720
#define VENC_CHN0	0
#define H264_KBPS	2048
#define H264_GOP	30

/* sensor mode: VICAP_SENSOR=1080 or 640, default 1280 (1280x960) */
static int sensor_mode(void)
{
	int m = getenv("VICAP_SENSOR") ? atoi(getenv("VICAP_SENSOR")) : 1280;

	return m == 1080 ? RC_SENSOR_1920X1080 : m == 640 ? RC_SENSOR_640X480 : RC_SENSOR_1280X960;
}

/* SYS/VB and the camera, through recamera-cam (shared with recamerad) */
static int vi_init(SIZE_S *size)
{
	struct rc_pool pool[3];
	unsigned int npool, blk;
	int sensor = sensor_mode();
	CVI_S32 r;

	r = rc_cam_size(sensor, &size->u32Width, &size->u32Height, &blk);
	if (r) { printf("rc_cam_size 0x%x\n", r); return r; }

	memset(pool, 0, sizeof(pool));
	if (g_use_vpss) {
		/* pools in ascending block size: the VB takes the first one that
		 * fits, so each channel lands in its own exact-size pool */
		npool = 3;
		pool[0].blk_size = rc_buffer_size(YOLO_W, YOLO_H, CH0_FMT);
		/* blocks per VPSS output pool (F7 step 10): 4 keeps the stream at
		 * 30 fps and YOLO at its full rate under the whole application
		 * load; 3 gives 23 fps, 2 gives 15, 1 stalls. VICAP_VPSS_BLKS=n */
		pool[0].blk_cnt = getenv("VICAP_VPSS_BLKS") ? atoi(getenv("VICAP_VPSS_BLKS")) : 4;
		pool[1].blk_size = rc_buffer_size(STREAM_W, STREAM_H, PIXEL_FORMAT_NV21);
		pool[1].blk_cnt = pool[0].blk_cnt;
		pool[2].blk_size = blk;
		/* encoding: one 1080p block less (3.1 MB of ION) leaves room for
		 * the encoder's reference frames and bitstream buffer */
		pool[2].blk_cnt = g_enc != PT_BUTT || g_yolo || g_all ? 3 : 4;
		/* online: the VI writes no frames, the 1080p pool is not needed */
		if (g_online)
			npool = 2;
	} else {
		npool = 1;
		pool[0].blk_size = blk;
		pool[0].blk_cnt = 5;
	}
	for (unsigned int i = 0; i < npool; i++)
		printf("VB pool %u: %u x %u bytes\n", i, pool[i].blk_cnt, pool[i].blk_size);

	r = rc_sys_open(pool, npool, g_online);
	if (r) { printf("rc_sys_open 0x%x\n", r); return r; }
	if (g_online)
		printf("VI -> VPSS online (VPSS dev 1, no VI frame pool)\n");
	r = rc_cam_open(sensor);
	if (r) { printf("rc_cam_open 0x%x\n", r); rc_sys_close(); return r; }
	return 0;
}

static void vi_deinit(void)
{
	rc_cam_close();
	rc_sys_close();
}

static void vpss_stop(void);
static void enc_stop_all(void);

/* not async-signal-safe, like the vendor samples: a test tool's best effort */
static void on_signal(int sig)
{
	(void)sig;
	enc_stop_all();
	if (g_use_vpss)
		vpss_stop();
	vi_deinit();
	_exit(1);
}

/* write every plane of a frame (by its length) to path */
static int write_frame(VIDEO_FRAME_INFO_S *f, const char *path)
{
	CVI_U32 total = f->stVFrame.u32Length[0] + f->stVFrame.u32Length[1] +
			f->stVFrame.u32Length[2];
	CVI_U8 *va;
	FILE *out;

	printf("%s: %ux%u fmt %d stride %u/%u/%u len %u/%u/%u\n", path,
	       f->stVFrame.u32Width, f->stVFrame.u32Height, f->stVFrame.enPixelFormat,
	       f->stVFrame.u32Stride[0], f->stVFrame.u32Stride[1], f->stVFrame.u32Stride[2],
	       f->stVFrame.u32Length[0], f->stVFrame.u32Length[1], f->stVFrame.u32Length[2]);
	va = CVI_SYS_Mmap(f->stVFrame.u64PhyAddr[0], total);
	CVI_SYS_IonInvalidateCache(f->stVFrame.u64PhyAddr[0], va, total);
	out = fopen(path, "wb");
	if (out) {
		fwrite(va, total, 1, out);
		fclose(out);
	}
	CVI_SYS_Munmap(va, total);
	return out ? 0 : -1;
}

static int vpss_start(const SIZE_S *in)
{
	VPSS_GRP_ATTR_S g;
	VPSS_CHN_ATTR_S c;
	MMF_CHN_S src = { .enModId = CVI_ID_VI, .s32DevId = 0, .s32ChnId = 0 };
	MMF_CHN_S dst = { .enModId = CVI_ID_VPSS, .s32DevId = VPSS_GRP0, .s32ChnId = 0 };
	CVI_S32 r;

	memset(&g, 0, sizeof(g));
	g.u32MaxW = in->u32Width;
	g.u32MaxH = in->u32Height;
	g.enPixelFormat = PIXEL_FORMAT_NV21;
	g.stFrameRate.s32SrcFrameRate = -1;
	g.stFrameRate.s32DstFrameRate = -1;
	g.u8VpssDev = g_online ? 1 : 0;
	r = CVI_VPSS_CreateGrp(VPSS_GRP0, &g);
	if (r) { printf("VPSS_CreateGrp 0x%x\n", r); return r; }
	CVI_VPSS_ResetGrp(VPSS_GRP0);

	/* ch0: YOLO input, 640x640 RGB planar, aspect kept, grey (114) bars */
	memset(&c, 0, sizeof(c));
	c.u32Width = YOLO_W;
	c.u32Height = YOLO_H;
	c.enVideoFormat = VIDEO_FORMAT_LINEAR;
	c.enPixelFormat = CH0_FMT;
	c.stFrameRate.s32SrcFrameRate = -1;
	c.stFrameRate.s32DstFrameRate = -1;
	c.u32Depth = 1;
	c.stAspectRatio.enMode = ASPECT_RATIO_AUTO;
	c.stAspectRatio.bEnableBgColor = CVI_TRUE;
	c.stAspectRatio.u32BgColor = 0x727272;
	r = CVI_VPSS_SetChnAttr(VPSS_GRP0, 0, &c);
	if (!r)
		r = CVI_VPSS_EnableChn(VPSS_GRP0, 0);
	if (r) { printf("VPSS ch0 0x%x\n", r); return r; }

	/* ch1: stream, 1280x720 NV21 */
	c.u32Width = STREAM_W;
	c.u32Height = STREAM_H;
	c.enPixelFormat = PIXEL_FORMAT_NV21;
	c.stAspectRatio.enMode = ASPECT_RATIO_NONE;
	c.stAspectRatio.bEnableBgColor = CVI_FALSE;
	r = CVI_VPSS_SetChnAttr(VPSS_GRP0, 1, &c);
	if (!r && in->u32Width * 9 != in->u32Height * 16) {
		/* a 4:3 sensor mode: the 16:9 stream is its centre band, not a
		 * squeezed picture (ch0, the YOLO input, keeps the whole frame) */
		VPSS_CROP_INFO_S crop = { .bEnable = CVI_TRUE, .enCropCoordinate = VPSS_CROP_ABS_COOR };

		crop.stCropRect.u32Width = in->u32Width;
		crop.stCropRect.u32Height = (in->u32Width * 9 / 16) & ~1u;
		crop.stCropRect.s32X = 0;
		crop.stCropRect.s32Y = ((in->u32Height - crop.stCropRect.u32Height) / 2) & ~1u;
		r = CVI_VPSS_SetChnCrop(VPSS_GRP0, 1, &crop);
		printf("VPSS ch1: 16:9 crop %ux%u at y %d of %ux%u\n", crop.stCropRect.u32Width,
		       crop.stCropRect.u32Height, crop.stCropRect.s32Y, in->u32Width, in->u32Height);
	}
	if (!r)
		r = CVI_VPSS_EnableChn(VPSS_GRP0, 1);
	if (r) { printf("VPSS ch1 0x%x\n", r); return r; }

	r = CVI_VPSS_StartGrp(VPSS_GRP0);
	if (r) { printf("VPSS_StartGrp 0x%x\n", r); return r; }
	if (g_online)
		return 0;	/* the ISP feeds the group directly, no bind */
	r = CVI_SYS_Bind(&src, &dst);
	if (r) { printf("SYS_Bind VI->VPSS 0x%x\n", r); return r; }
	return 0;
}

static void vpss_stop(void)
{
	MMF_CHN_S src = { .enModId = CVI_ID_VI, .s32DevId = 0, .s32ChnId = 0 };
	MMF_CHN_S dst = { .enModId = CVI_ID_VPSS, .s32DevId = VPSS_GRP0, .s32ChnId = 0 };

	if (!g_online)
		CVI_SYS_UnBind(&src, &dst);
	CVI_VPSS_DisableChn(VPSS_GRP0, 0);
	CVI_VPSS_DisableChn(VPSS_GRP0, 1);
	CVI_VPSS_StopGrp(VPSS_GRP0);
	CVI_VPSS_DestroyGrp(VPSS_GRP0);
}

static int save_vpss(const char *path)
{
	VIDEO_FRAME_INFO_S f;
	char name[256];
	int r = 0;

	for (int ch = 0; ch < 2; ch++) {
		if (CVI_VPSS_GetChnFrame(VPSS_GRP0, ch, &f, 3000)) {
			printf("VPSS ch%d: GetChnFrame failed\n", ch);
			r = -1;
			continue;
		}
		snprintf(name, sizeof(name), "%s.ch%d.%s", path, ch, ch ? "nv21" : "rgbp");
		r |= write_frame(&f, name);
		CVI_VPSS_ReleaseChnFrame(VPSS_GRP0, ch, &f);
	}
	return r;
}

/* encoder channels started (stopped by enc_stop_all, also on a signal) */
static int g_enc_on[2];

static int enc_start(VENC_CHN chn, PAYLOAD_TYPE_E type)
{
	VENC_CHN_ATTR_S a;
	VENC_JPEG_PARAM_S jp;
	VENC_RECV_PIC_PARAM_S recv = { .s32RecvPicNum = -1 };
	CVI_S32 r;

	memset(&a, 0, sizeof(a));
	a.stVencAttr.enType = type;
	a.stVencAttr.u32MaxPicWidth = STREAM_W;
	a.stVencAttr.u32MaxPicHeight = STREAM_H;
	a.stVencAttr.u32PicWidth = STREAM_W;
	a.stVencAttr.u32PicHeight = STREAM_H;
	a.stVencAttr.bByFrame = CVI_TRUE;
	/* bitstream buffer: 0 means the vendor default (JPEG: 9 MiB, sized for
	 * 4096x2304), which does not fit the ION heap next to VI/VPSS; the
	 * failed allocation then hung the SoC (F7, 19/10/2026). 1 MiB is ~5x
	 * a 720p Q85 JPEG and ~4 s of H.264 at 2 Mbit/s. */
	a.stVencAttr.u32BufSize = 1024 * 1024;
	a.stGopAttr.enGopMode = VENC_GOPMODE_NORMALP;
	if (type == PT_JPEG) {
		a.stVencAttr.stAttrJpege.enReceiveMode = VENC_PIC_RECEIVE_SINGLE;
	} else {
		VENC_H264_CBR_S *cbr = &a.stRcAttr.stH264Cbr;

		/* Baseline: the H.264 profile every WebRTC browser decodes */
		a.stVencAttr.u32Profile = H264E_PROFILE_BASELINE;
		a.stRcAttr.enRcMode = VENC_RC_MODE_H264CBR;
		cbr->u32Gop = H264_GOP;
		cbr->u32StatTime = 2;
		cbr->u32SrcFrameRate = 30;
		cbr->fr32DstFrameRate = 30;
		cbr->u32BitRate = H264_KBPS;
		a.stGopAttr.stNormalP.s32IPQpDelta = 2;
	}
	printf("enc %d: CreateChn\n", chn);
	r = CVI_VENC_CreateChn(chn, &a);
	if (r) { printf("VENC_CreateChn 0x%x\n", r); return r; }
	g_enc_on[chn] = 1;

	if (type == PT_H264) {
		/* The SPS profile is derived from the entropy coder (CABAC ->
		 * Main), and the encoder starts with CABAC whatever u32Profile
		 * says: the channel default computed for Baseline reaches the
		 * encoder only through this call (as the vendor sample does). */
		VENC_H264_ENTROPY_S e = {
			.u32EntropyEncModeI = H264E_ENTROPY_CAVLC,
			.u32EntropyEncModeP = H264E_ENTROPY_CAVLC,
		};

		r = CVI_VENC_SetH264Entropy(chn, &e);
		if (r) { printf("VENC_SetH264Entropy 0x%x\n", r); return r; }
	}
	if (type == PT_JPEG && CVI_VENC_GetJpegParam(chn, &jp) == 0) {
		jp.u32Qfactor = 85;
		CVI_VENC_SetJpegParam(chn, &jp);
	}
	printf("enc %d: StartRecvFrame\n", chn);
	r = CVI_VENC_StartRecvFrame(chn, &recv);
	if (r) { printf("VENC_StartRecvFrame 0x%x\n", r); return r; }
	return 0;
}

static void enc_stop_all(void)
{
	for (VENC_CHN chn = 0; chn < 2; chn++) {
		if (!g_enc_on[chn])
			continue;
		CVI_VENC_StopRecvFrame(chn);
		CVI_VENC_ResetChn(chn);
		CVI_VENC_DestroyChn(chn);
		g_enc_on[chn] = 0;
	}
}

/* one frame through encoder chn; the stream goes to out (NULL: discarded),
 * its size to *len */
static int enc_frame(VENC_CHN chn, const VIDEO_FRAME_INFO_S *f, FILE *out, CVI_U32 *len)
{
	VENC_CHN_STATUS_S st;
	VENC_STREAM_S s;
	CVI_S32 r;

	*len = 0;
	r = CVI_VENC_SendFrame(chn, f, 3000);
	if (r) { printf("VENC_SendFrame %d 0x%x\n", chn, r); return r; }

	r = CVI_VENC_QueryStatus(chn, &st);
	if (r || !st.u32CurPacks) { printf("VENC_QueryStatus 0x%x packs %u\n", r, st.u32CurPacks); return -1; }
	memset(&s, 0, sizeof(s));
	s.pstPack = calloc(st.u32CurPacks, sizeof(VENC_PACK_S));
	r = CVI_VENC_GetStream(chn, &s, 3000);
	if (r) { printf("VENC_GetStream 0x%x\n", r); free(s.pstPack); return r; }
	for (CVI_U32 i = 0; i < s.u32PackCount; i++) {
		VENC_PACK_S *p = &s.pstPack[i];

		*len += p->u32Len - p->u32Offset;
		if (getenv("VICAP_PACKS") && s.u32Seq < 2)
			printf("seq %u pack %u: phy 0x%llx va %p len %u off %u: %02x %02x %02x %02x %02x %02x\n",
			       s.u32Seq, i, (unsigned long long)p->u64PhyAddr, p->pu8Addr, p->u32Len, p->u32Offset,
			       p->pu8Addr[0], p->pu8Addr[1], p->pu8Addr[2], p->pu8Addr[3], p->pu8Addr[4], p->pu8Addr[5]);
		if (out)
			fwrite(p->pu8Addr + p->u32Offset, p->u32Len - p->u32Offset, 1, out);
	}
	CVI_VENC_ReleaseStream(chn, &s);
	free(s.pstPack);
	return 0;
}

/* one VPSS ch1 frame through encoder 0 */
static int enc_one(FILE *out, CVI_U32 *len)
{
	VIDEO_FRAME_INFO_S f;
	int r;

	*len = 0;
	if (CVI_VPSS_GetChnFrame(VPSS_GRP0, 1, &f, 3000)) {
		printf("enc: GetChnFrame failed\n");
		return -1;
	}
	r = enc_frame(VENC_CHN0, &f, out, len);
	CVI_VPSS_ReleaseChnFrame(VPSS_GRP0, 1, &f);
	return r;
}

/* JPEG: n frames, the last one saved; H.264: all n frames saved */
static int save_enc(const char *path, int n)
{
	struct timespec t0, t1;
	char name[256];
	CVI_U32 len, total = 0, first = 0;
	FILE *out;
	double dt;
	int i, jpeg = g_enc == PT_JPEG;

	snprintf(name, sizeof(name), "%s.%s", path, jpeg ? "jpg" : "h264");
	out = fopen(name, "wb");
	if (!out)
		return -1;
	clock_gettime(CLOCK_MONOTONIC, &t0);
	for (i = 0; i < n; i++) {
		/* VICAP_AE_EVERY=n: the ISP's exposure every n frames, to see how
		 * the auto exposure answers a light change (LED PWM fade test) */
		int ae_every = getenv("VICAP_AE_EVERY") ? atoi(getenv("VICAP_AE_EVERY")) : 0;

		if (ae_every > 0 && i % ae_every == 0) {
			ISP_EXP_INFO_S x;
			struct timespec t;

			clock_gettime(CLOCK_MONOTONIC, &t);
			if (CVI_ISP_QueryExposureInfo(0, &x) == 0)
				printf("ae %d t=%.2f: exposure %u us, again %u, dgain %u, ispgain %u, "
				       "ISO %u, luma %u, LV %.2f\n", i,
				       (t.tv_sec - t0.tv_sec) + (t.tv_nsec - t0.tv_nsec) / 1e9,
				       x.u32ExpTime, x.u32AGain, x.u32DGain, x.u32ISPDGain,
				       x.u32ISO, x.u8AveLum, x.fLightValue);
		}
		if (jpeg && i == n - 1)
			rewind(out);
		if (enc_one(!jpeg || i == n - 1 ? out : NULL, &len))
			break;
		if (!i)
			first = len;
		total += len;
	}
	clock_gettime(CLOCK_MONOTONIC, &t1);
	fclose(out);
	if (i < n)
		return -1;
	dt = (t1.tv_sec - t0.tv_sec) + (t1.tv_nsec - t0.tv_nsec) / 1e9;
	printf("enc: %d frames %ux%u in %.2f s = %.1f fps, first %u bytes, avg %u bytes (%.0f kbit/s); saved %s\n",
	       n, STREAM_W, STREAM_H, dt, n / dt, first, total / n, total * 8 / dt / 1000, name);
	return 0;
}

#ifdef RECAMERA_NPU
static int run_yolo(const char *path, int n, const char *model, int target, float thr)
{
	struct yolo_det d[64];
	struct timespec t0, t1, a, b;
	double npu = 0, dt;
	char name[256];
	int in_w, in_h, i, k, nd = 0, frames_with = 0;
	FILE *out;

	if (yolo_open(model, &in_w, &in_h))
		return -1;
	if (in_w != YOLO_W || in_h != YOLO_H) {
		printf("yolo: model input %dx%d, VPSS ch0 gives %dx%d\n", in_w, in_h, YOLO_W, YOLO_H);
		yolo_close();
		return -1;
	}
	clock_gettime(CLOCK_MONOTONIC, &t0);
	for (i = 0; i < n; i++) {
		VIDEO_FRAME_INFO_S f;

		if (CVI_VPSS_GetChnFrame(VPSS_GRP0, 0, &f, 3000)) {
			printf("yolo: GetChnFrame failed\n");
			break;
		}
		clock_gettime(CLOCK_MONOTONIC, &a);
		nd = yolo_run(f.stVFrame.u64PhyAddr[0], target, thr, 0.45f, d, 64);
		clock_gettime(CLOCK_MONOTONIC, &b);
		npu += (b.tv_sec - a.tv_sec) * 1e3 + (b.tv_nsec - a.tv_nsec) / 1e6;
		if (nd > 0)
			frames_with++;
		if (nd >= 0 && (i < 3 || i == n - 1 || nd != 0))
			for (k = 0; k < nd; k++)
				printf("frame %d: class %d score %.2f box %.0f,%.0f-%.0f,%.0f\n", i, d[k].cls,
				       d[k].score, d[k].x1, d[k].y1, d[k].x2, d[k].y2);
		if (i == n - 1) {
			snprintf(name, sizeof(name), "%s.rgb", path);
			write_frame(&f, name);
			snprintf(name, sizeof(name), "%s.det", path);
			out = fopen(name, "w");
			for (k = 0; out && k < nd; k++)
				fprintf(out, "%d %.3f %.1f %.1f %.1f %.1f\n", d[k].cls, d[k].score,
					d[k].x1, d[k].y1, d[k].x2, d[k].y2);
			if (out)
				fclose(out);
		}
		CVI_VPSS_ReleaseChnFrame(VPSS_GRP0, 0, &f);
		if (nd < 0)
			break;
	}
	clock_gettime(CLOCK_MONOTONIC, &t1);
	dt = (t1.tv_sec - t0.tv_sec) + (t1.tv_nsec - t0.tv_nsec) / 1e9;
	printf("yolo: %d frames in %.2f s = %.1f fps; inference+decode %.1f ms/frame; "
	       "class %d found in %d frames\n", i, dt, i / dt, i ? npu / i : 0, target, frames_with);
	yolo_close();
	return i == n ? 0 : -1;
}
#endif

#ifdef RECAMERA_NPU
/*
 * "all": the application's load. A thread takes every VPSS ch1 frame (720p)
 * to the H.264 encoder (channel 0, stream to <out>.h264) and to the JPEG
 * encoder (channel 1, the MJPEG load, discarded), while the main thread runs
 * YOLO on ch0 (run_yolo). Both rates are printed.
 */
struct enc_load {
	volatile int stop;
	int frames, err;
	unsigned long h264_bytes, jpeg_bytes;
	FILE *h264;
};

static void *enc_load_thread(void *arg)
{
	struct enc_load *e = arg;

	while (!e->stop) {
		VIDEO_FRAME_INFO_S f;
		CVI_U32 l0, l1;

		if (CVI_VPSS_GetChnFrame(VPSS_GRP0, 1, &f, 1000))
			continue;
		if (enc_frame(0, &f, e->h264, &l0) || enc_frame(1, &f, NULL, &l1))
			e->err++;
		else {
			e->frames++;
			e->h264_bytes += l0;
			e->jpeg_bytes += l1;
		}
		CVI_VPSS_ReleaseChnFrame(VPSS_GRP0, 1, &f);
	}
	return NULL;
}

static int run_all(const char *path, int n, const char *model, int target, float thr)
{
	struct enc_load e = { 0 };
	struct timespec t0, t1;
	char name[256];
	pthread_t th;
	double dt;
	int r;

	if (enc_start(0, PT_H264) || enc_start(1, PT_JPEG))
		return -1;
	snprintf(name, sizeof(name), "%s.h264", path);
	e.h264 = fopen(name, "wb");
	clock_gettime(CLOCK_MONOTONIC, &t0);
	pthread_create(&th, NULL, enc_load_thread, &e);
	r = run_yolo(path, n, model, target, thr);
	e.stop = 1;
	pthread_join(th, NULL);
	clock_gettime(CLOCK_MONOTONIC, &t1);
	if (e.h264)
		fclose(e.h264);
	dt = (t1.tv_sec - t0.tv_sec) + (t1.tv_nsec - t0.tv_nsec) / 1e9;
	printf("all: %d stream frames in %.2f s = %.1f fps to H.264 (%.0f kbit/s) and JPEG "
	       "(%.0f kbit/s), %d errors\n", e.frames, dt, e.frames / dt,
	       e.h264_bytes * 8 / dt / 1000, e.jpeg_bytes * 8 / dt / 1000, e.err);
	return r || e.err ? -1 : 0;
}
#endif

static int save_frame(const char *path)
{
	VIDEO_FRAME_INFO_S f;
	FILE *out;
	CVI_U32 total, luma, chroma;
	CVI_U8 *va;

	if (CVI_VI_GetChnFrame(0, 0, &f, 3000)) {
		printf("GetChnFrame failed\n");
		return -1;
	}
	total = f.stVFrame.u32Length[0] + f.stVFrame.u32Length[1] + f.stVFrame.u32Length[2];
	luma = f.stVFrame.u32Stride[0] * f.stVFrame.u32Height;
	chroma = f.stVFrame.u32Stride[1] * f.stVFrame.u32Height / 2;
	printf("frame %ux%u fmt %d stride %u/%u len %u/%u/%u\n",
	       f.stVFrame.u32Width, f.stVFrame.u32Height, f.stVFrame.enPixelFormat,
	       f.stVFrame.u32Stride[0], f.stVFrame.u32Stride[1],
	       f.stVFrame.u32Length[0], f.stVFrame.u32Length[1], f.stVFrame.u32Length[2]);

	va = CVI_SYS_Mmap(f.stVFrame.u64PhyAddr[0], total);
	CVI_SYS_IonInvalidateCache(f.stVFrame.u64PhyAddr[0], va, total);
	out = fopen(path, "wb");
	if (out) {
		fwrite(va, luma, 1, out);
		if (f.stVFrame.u32Length[1])
			fwrite(va + f.stVFrame.u32Length[0], chroma, 1, out);
		fclose(out);
		printf("saved %s (%u + %u bytes)\n", path, luma, chroma);
	}
	CVI_SYS_Munmap(va, total);
	CVI_VI_ReleaseChnFrame(0, 0, &f);
	return out ? 0 : -1;
}

int main(int argc, char **argv)
{
	int skip = argc > 1 ? atoi(argv[1]) : 30;
	const char *path = argc > 2 ? argv[2] : "/data/vicap.yuv";
	const char *mode = argc > 3 ? argv[3] : "";
	g_enc = !strcmp(mode, "jpeg") ? PT_JPEG : !strcmp(mode, "h264") ? PT_H264 : PT_BUTT;
	g_yolo = !strcmp(mode, "yolo");
	g_all = !strcmp(mode, "all");
	g_use_vpss = g_enc != PT_BUTT || g_yolo || g_all || !strcmp(mode, "vpss");
	int nenc = argc > 4 ? atoi(argv[4]) : g_enc == PT_JPEG || g_yolo || g_all ? 30 : 150;
	SIZE_S size;
	ISP_EXP_INFO_S exp;
	int i, r;

	setvbuf(stdout, NULL, _IONBF, 0); /* progress survives a crash */
	/* online needs the VPSS: the plain capture reads the VI's own frames */
	g_online = g_use_vpss && (!getenv("VICAP_ONLINE") || atoi(getenv("VICAP_ONLINE")));
	/* every way the process can be killed must run the MPI teardown: the VB
	 * pools and VI buffers live in the kernel and outlive the process
	 * (ION stays held until a reboot, F7 step 9) */
	signal(SIGINT, on_signal);
	signal(SIGTERM, on_signal);
	signal(SIGHUP, on_signal);
	signal(SIGPIPE, on_signal);

	/* what a killed run left in the kernel */
	r = rc_reclaim();
	if (r < 0) {
		printf("the camera is in use by pid %d\n", -r);
		return 1;
	}
	if (r)
		printf("reclaimed %d objects of a previous run\n", r);

	r = vi_init(&size);
	if (!r && g_use_vpss)
		r = vpss_start(&size);
	if (!r && g_enc != PT_BUTT)
		r = enc_start(VENC_CHN0, g_enc);
	if (r) {
		enc_stop_all();
		if (g_use_vpss)
			vpss_stop();
		vi_deinit();
		return 1;
	}

	/* let AE/AWB converge */
	for (i = 0; i < skip; i++) {
		VIDEO_FRAME_INFO_S f;

		if (g_use_vpss) {
			if (CVI_VPSS_GetChnFrame(VPSS_GRP0, 1, &f, 3000)) {
				printf("vpss frame %d: timeout\n", i);
				break;
			}
			CVI_VPSS_ReleaseChnFrame(VPSS_GRP0, 1, &f);
			continue;
		}
		if (CVI_VI_GetChnFrame(0, 0, &f, 3000)) {
			printf("frame %d: timeout\n", i);
			break;
		}
		CVI_VI_ReleaseChnFrame(0, 0, &f);
	}
	printf("skipped %d frames\n", i);

	if (CVI_ISP_QueryExposureInfo(0, &exp) == 0)
		printf("AE: exposure %u us, again %u, dgain %u, ispgain %u, ISO %u, "
		       "avg luma %u, LV %.2f, fps %u\n",
		       exp.u32ExpTime, exp.u32AGain, exp.u32DGain, exp.u32ISPDGain,
		       exp.u32ISO, exp.u8AveLum, exp.fLightValue, exp.u32Fps);

	if (g_yolo || g_all) {
#ifdef RECAMERA_NPU
		const char *model = argc > 5 ? argv[5] : "/data/models/yolo11n_detection_cv181x_int8.cvimodel";
		int target = argc > 6 ? atoi(argv[6]) : 0;
		float thr = argc > 7 ? atof(argv[7]) : 0.5f;

		if (g_all)
			r = run_all(path, nenc < 1 ? 1 : nenc, model, target, thr);
		else
			r = run_yolo(path, nenc < 1 ? 1 : nenc, model, target, thr);
		enc_stop_all();
#else
		printf("built without the NPU runtime (sophgo-cviruntime)\n");
		r = -1;
#endif
		vpss_stop();
	} else if (g_enc != PT_BUTT) {
		r = save_enc(path, nenc < 1 ? 1 : nenc);
		enc_stop_all();
		vpss_stop();
	} else if (g_use_vpss) {
		r = save_vpss(path);
		vpss_stop();
	} else {
		r = save_frame(path);
	}
	vi_deinit();
	return r ? 1 : 0;
}
