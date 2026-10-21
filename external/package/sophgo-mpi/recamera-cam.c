// SPDX-License-Identifier: MIT OR Apache-2.0
/*
 * recamera-cam -- the reCamera camera bring-up (recamera-cam.h). The sensor
 * and VI sequence is the vendor sample_sensor_test / SAMPLE_PLAT_VI_INIT one,
 * with the sensor config filled in code (no sensor_cfg.ini); linked with the
 * vendor sample_common_{sys,vi,isp,bin,sensor}.c and inih.
 */
#include <dirent.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "sample_comm.h"
#include "cvi_isp.h"
#include "cvi_vpss.h"
#include "cvi_venc.h"
#include "linux/cvi_vc_drv_ioctl.h"
#include "recamera-cam.h"

/* The structs the Rust side (app/sophgo-sys) declares by hand: a header
 * change must fail this build, not corrupt memory at run time. */
_Static_assert(sizeof(VIDEO_FRAME_S) == 144, "VIDEO_FRAME_S");
_Static_assert(sizeof(VIDEO_FRAME_INFO_S) == 152, "VIDEO_FRAME_INFO_S");
_Static_assert(offsetof(VIDEO_FRAME_S, u64PhyAddr) == 48, "VIDEO_FRAME_S.u64PhyAddr");
_Static_assert(offsetof(VIDEO_FRAME_S, u32Length) == 96, "VIDEO_FRAME_S.u32Length");
_Static_assert(offsetof(VIDEO_FRAME_S, u64PTS) == 120, "VIDEO_FRAME_S.u64PTS");
_Static_assert(offsetof(VIDEO_FRAME_S, u32FrameFlag) == 136, "VIDEO_FRAME_S.u32FrameFlag");
_Static_assert(sizeof(VPSS_GRP_ATTR_S) == 24, "VPSS_GRP_ATTR_S");
_Static_assert(offsetof(VPSS_GRP_ATTR_S, u8VpssDev) == 20, "VPSS_GRP_ATTR_S.u8VpssDev");
_Static_assert(sizeof(VPSS_CHN_ATTR_S) == 92, "VPSS_CHN_ATTR_S");
_Static_assert(offsetof(VPSS_CHN_ATTR_S, u32Depth) == 28, "VPSS_CHN_ATTR_S.u32Depth");
_Static_assert(offsetof(VPSS_CHN_ATTR_S, stAspectRatio) == 32, "VPSS_CHN_ATTR_S.stAspectRatio");
_Static_assert(offsetof(VPSS_CHN_ATTR_S, stNormalize) == 60, "VPSS_CHN_ATTR_S.stNormalize");
_Static_assert(sizeof(VPSS_CROP_INFO_S) == 24, "VPSS_CROP_INFO_S");
_Static_assert(PIXEL_FORMAT_RGB_888 == 0 && PIXEL_FORMAT_RGB_888_PLANAR == 2 &&
	       PIXEL_FORMAT_NV12 == 18 && PIXEL_FORMAT_NV21 == 19, "PIXEL_FORMAT_E");
_Static_assert(VIDEO_FORMAT_LINEAR == 0 && ASPECT_RATIO_NONE == 0 && ASPECT_RATIO_AUTO == 1 &&
	       VPSS_CROP_ABS_COOR == 1, "VPSS enums");

/* Referenced by the vendor sample_common_vi.c suspend/resume path only,
 * which is not used here (the real ones live in the VPSS/VO-heavy
 * sample_common_platform.c). */
CVI_S32 SAMPLE_PLAT_SYS_INIT(SIZE_S stSize)
{
	(void)stSize;
	return CVI_FAILURE;
}

CVI_S32 SAMPLE_PLAT_VI_INIT(SAMPLE_VI_CONFIG_S *pstViConfig)
{
	(void)pstViConfig;
	return CVI_FAILURE;
}

static SAMPLE_VI_CONFIG_S g_vi;
static SAMPLE_INI_CFG_S g_ini;
static int g_sys_open, g_cam_open;
/* libsys: the /dev/cvi-vi fd it opened (exported, not in the headers) */
int get_vi_fd(void);

/* the tuned sharpen gains, read once (rc_isp_image scales them) */
static CVI_U8 g_sharp_auto[ISP_AUTO_ISO_STRENGTH_NUM], g_sharp_manual;
static int g_sharp_saved;

/* a second reference to /dev/cvi-vi, high fd (see pin_vi_last) */
static int g_vi_pin = -1;

/*
 * A process killed without its teardown has its files closed by the kernel
 * in fd order, and libsys opens /dev/cvi-vi before /dev/cvi-vpss: the VI
 * release stopped the ISP while the online VPSS group still read from it,
 * the reverse of the teardown order, and that hung the SoC now and then
 * (A3, 20/10/2026: watchdog resets on kill -9, the log ends with VPSS
 * waiting for frames). A duplicate of the VI fd above every other fd makes
 * the VI file's last reference close after the VPSS one, so the drivers
 * stop VPSS first, then VI, as rc_cam_close does.
 */
static void pin_vi_last(void)
{
	int fd = get_vi_fd();

	if (fd >= 0 && g_vi_pin < 0)
		g_vi_pin = fcntl(fd, F_DUPFD_CLOEXEC, 900);
}

static void unpin_vi(void)
{
	if (g_vi_pin >= 0)
		close(g_vi_pin);
	g_vi_pin = -1;
}

/* reCamera OV5647: board wiring (camera I2C = i2c2, 0x36 measured; MCLK0
 * 24 MHz). MIPI lanes {2, 0, 3}: clock on physical lane 2, data on 0 and 3,
 * as the factory sscma-node configures it (/proc/mipi-rx LinkId on the
 * running factory system, 19/10/2026) -- NOT the {0, 1, 2} default of the
 * vendor ov5647_cmos_param.h, which gives no frames on this board. */
static int fill_cfg(int sensor, SAMPLE_INI_CFG_S *c)
{
	memset(c, 0, sizeof(*c));
	c->enSource = VI_PIPE_FRAME_SOURCE_DEV;
	c->devNum = 1;
	switch (sensor) {
	case RC_SENSOR_1280X960:
		c->enSnsType[0] = OV_OV5647_MIPI_1M2_30FPS_10BIT;
		break;
	case RC_SENSOR_1920X1080:
		c->enSnsType[0] = OV_OV5647_MIPI_2M_30FPS_10BIT;
		break;
	case RC_SENSOR_640X480:
		c->enSnsType[0] = OV_OV5647_MIPI_480P_30FPS_10BIT;
		break;
	default:
		return CVI_FAILURE;
	}
	c->enWDRMode[0] = WDR_MODE_NONE;
	c->s32BusId[0] = 2;
	c->s32SnsI2cAddr[0] = 0x36;
	c->MipiDev[0] = 0;
	c->as16LaneId[0][0] = 2;
	c->as16LaneId[0][1] = 0;
	c->as16LaneId[0][2] = 3;
	c->as16LaneId[0][3] = -1;
	c->as16LaneId[0][4] = -1;
	c->stMclkAttr[0].bMclkEn = 1;
	c->stMclkAttr[0].u8Mclk = 0;
	c->s16SwitchGpio[0] = -1;
	return 0;
}

int rc_cam_size(int sensor, unsigned int *width, unsigned int *height,
		unsigned int *vi_blk_size)
{
	SAMPLE_INI_CFG_S ini;
	SAMPLE_VI_CONFIG_S vi;
	PIC_SIZE_E pic;
	SIZE_S size;
	CVI_S32 r;

	r = fill_cfg(sensor, &ini);
	if (!r)
		r = SAMPLE_COMM_VI_IniToViCfg(&ini, &vi);
	if (!r)
		r = SAMPLE_COMM_VI_GetSizeBySensor(ini.enSnsType[0], &pic);
	if (!r)
		r = SAMPLE_COMM_SYS_GetPicSize(pic, &size);
	if (r)
		return r;
	*width = size.u32Width;
	*height = size.u32Height;
	if (vi_blk_size)
		*vi_blk_size = COMMON_GetPicBufferSize(size.u32Width, size.u32Height,
			vi.astViInfo[0].stChnInfo.enPixFormat, DATA_BITWIDTH_8,
			COMPRESS_MODE_NONE, DEFAULT_ALIGN);
	return 0;
}

unsigned int rc_buffer_size(unsigned int width, unsigned int height, int pixfmt)
{
	return COMMON_GetPicBufferSize(width, height, (PIXEL_FORMAT_E)pixfmt,
				       DATA_BITWIDTH_8, COMPRESS_MODE_NONE, DEFAULT_ALIGN);
}

/* sophgo-osdrv patch 0002: free the ION buffers a killed process left */
#define SYS_ION_FREE_ORPHANS _IO('S', 0x20)

/* libsys internals: exported by libsys.so, not in the public headers */
int get_base_fd(void);
int vb_ioctl_exit(int fd);
int vb_ioctl_get_vb_init(int fd, CVI_U32 *val);

/* pid of another process holding an MPI device open, or 0 */
static int mpi_user(void)
{
	static const char *const devs[] = { "/dev/cvi-base", "/dev/cvi-vi", "/dev/cvi-vpss" };
	char path[300], link[64];
	struct dirent *p, *f;
	DIR *proc, *fds;
	int self = getpid(), found = 0;

	proc = opendir("/proc");
	if (!proc)
		return 0;
	while (!found && (p = readdir(proc))) {
		int pid = atoi(p->d_name);

		if (pid <= 0 || pid == self)
			continue;
		snprintf(path, sizeof(path), "/proc/%d/fd", pid);
		fds = opendir(path);
		if (!fds)
			continue;
		while (!found && (f = readdir(fds))) {
			ssize_t n;

			snprintf(path, sizeof(path), "/proc/%d/fd/%s", pid, f->d_name);
			n = readlink(path, link, sizeof(link) - 1);
			if (n <= 0)
				continue;
			link[n] = 0;
			for (unsigned int i = 0; i < sizeof(devs) / sizeof(devs[0]); i++)
				if (!strcmp(link, devs[i]))
					found = pid;
		}
		closedir(fds);
	}
	closedir(proc);
	return found;
}

/*
 * The encoder channels that exist, as a bit mask, from the driver's proc
 * file ("ID: <n>\t Width: ..." under "VENC CHN ATTR 1"). The reclaim only
 * opens the device of a channel that exists: the per-channel ioctl takes a
 * semaphore that was once found held with no holder left (A3, 20/10/2026),
 * and a reclaim blocked there blocked the whole pipeline start.
 */
static unsigned int venc_channels(void)
{
	char line[256];
	unsigned int mask = 0;
	FILE *f = fopen("/proc/cvitek/venc", "r");
	int id;

	if (!f)
		return 0;
	while (fgets(line, sizeof(line), f))
		if (sscanf(line, "ID: %d\t Width:", &id) == 1 && strstr(line, "Width:") &&
		    id >= 0 && id < 32)
			mask |= 1u << id;
	fclose(f);
	return mask;
}

/*
 * The MPI keeps its objects in the kernel drivers, which do not release them
 * when the owning process dies (F7: ~20 MB of ION held and VPSS_CreateGrp
 * 0xc0068004 "exists" after a kill -9). A new process can destroy them by
 * id:
 * - encoder channels: through their devices (libvenc only uses its own fds),
 *   only the ones the driver lists;
 * - VPSS groups;
 * - VB pools: reference counted in the kernel (one per CVI_VB_Init, cleaned
 *   when the count reaches 0, blocks in use or not), and CVI_VB_Exit only
 *   reaches the kernel in a process that called CVI_VB_Init: the dead
 *   process' reference is dropped with the exit ioctl itself;
 * - the userspace ION buffers (VI_DMA_BUF, ISP_SHARED_BUFFER): orphaned by
 *   the sys driver when their process died, freed here (osdrv patch 0002).
 * The VI hardware needs nothing: the VI driver stops it when the last
 * /dev/cvi-vi file closes, as it did with the dead process.
 * Only when no live process uses the MPI: never tear down a running
 * pipeline (recamera-vicap started next to recamerad), and the orphans are
 * only safe to free once the VI is stopped.
 */
int rc_reclaim(void)
{
	VPSS_GRP_ATTR_S ga;
	CVI_U32 refs;
	unsigned int venc;
	int n = 0, pid, fd;

	pid = mpi_user();
	if (pid)
		return -pid;
	if (CVI_SYS_Init())
		return 0;
	/* the encoder channels live in cvi_vc_driver, one device per channel;
	 * libvenc only talks to the fds it opened itself, so ask the devices */
	venc = venc_channels();
	for (VENC_CHN c = 0; c < VENC_MAX_CHN_NUM && venc; c++) {
		VENC_CHN_ATTR_S a;
		char dev[32];

		if (!(venc & (1u << c)))
			continue;
		venc &= ~(1u << c);
		snprintf(dev, sizeof(dev), "/dev/%s%d", CVI_VC_DRV_ENCODER_DEV_NAME, c);
		fd = open(dev, O_RDWR | O_CLOEXEC);
		if (fd < 0)
			break;	/* past the last channel (MaxVencChnNum) */
		if (!ioctl(fd, CVI_VC_VENC_GET_CHN_ATTR, &a)) {
			ioctl(fd, CVI_VC_VENC_STOP_RECV_FRAME);
			ioctl(fd, CVI_VC_VENC_RESET_CHN);
			if (!ioctl(fd, CVI_VC_VENC_DESTROY_CHN))
				n++;
		}
		close(fd);
	}
	for (VPSS_GRP g = 0; g < VPSS_MAX_GRP_NUM; g++) {
		if (CVI_VPSS_GetGrpAttr(g, &ga))
			continue;
		for (VPSS_CHN c = 0; c < VPSS_MAX_CHN_NUM; c++)
			CVI_VPSS_DisableChn(g, c);
		CVI_VPSS_StopGrp(g);
		if (!CVI_VPSS_DestroyGrp(g))
			n++;
	}
	fd = get_base_fd();
	for (int i = 0; fd >= 0 && i < 16; i++) {
		if (vb_ioctl_get_vb_init(fd, &refs) || !refs)
			break;
		vb_ioctl_exit(fd);
		n++;
	}
	CVI_SYS_Exit();

	fd = open("/dev/cvi-sys", O_RDWR | O_CLOEXEC);
	if (fd >= 0) {
		int r = ioctl(fd, SYS_ION_FREE_ORPHANS);

		if (r > 0)
			n += r;
		close(fd);
	}
	return n;
}

int rc_sys_open(const struct rc_pool *pools, unsigned int npools, int online)
{
	VB_CONFIG_S vb;
	CVI_S32 r;

	if (npools > VB_MAX_COMM_POOLS)
		return CVI_FAILURE;
	memset(&vb, 0, sizeof(vb));
	vb.u32MaxPoolCnt = npools;
	for (unsigned int i = 0; i < npools; i++) {
		vb.astCommPool[i].u32BlkSize = pools[i].blk_size;
		vb.astCommPool[i].u32BlkCnt = pools[i].blk_cnt;
		vb.astCommPool[i].enRemapMode = VB_REMAP_MODE_CACHED;
	}
	r = SAMPLE_COMM_SYS_Init(&vb);
	if (r)
		return r;
	g_sys_open = 1;

	if (online) {
		/* as the SDK's sample_venc_lib.c _sys_config_online_mode() */
		VI_VPSS_MODE_S vvm;
		VPSS_MODE_S vm;

		memset(&vvm, 0, sizeof(vvm));
		memset(&vm, 0, sizeof(vm));
		vm.enMode = VPSS_MODE_DUAL;
		vm.aenInput[0] = VPSS_INPUT_MEM;
		vm.aenInput[1] = VPSS_INPUT_ISP;
		vvm.aenMode[0] = vvm.aenMode[1] = VI_OFFLINE_VPSS_ONLINE;
		r = CVI_SYS_SetVIVPSSMode(&vvm);
		if (!r)
			r = CVI_SYS_SetVPSSModeEx(&vm);
		if (r) {
			rc_sys_close();
			return r;
		}
	}
	return 0;
}

void rc_sys_close(void)
{
	if (!g_sys_open)
		return;
	SAMPLE_COMM_SYS_Exit();
	g_sys_open = 0;
}

int rc_cam_open(int sensor)
{
	VI_PIPE_ATTR_S pipe;
	PIC_SIZE_E pic;
	SIZE_S size;
	CVI_S32 r;

	r = fill_cfg(sensor, &g_ini);
	if (r)
		return r;
	CVI_VI_SetDevNum(g_ini.devNum);
	r = SAMPLE_COMM_VI_IniToViCfg(&g_ini, &g_vi);
	if (!r)
		r = SAMPLE_COMM_VI_GetSizeBySensor(g_ini.enSnsType[0], &pic);
	if (!r)
		r = SAMPLE_COMM_SYS_GetPicSize(pic, &size);
	if (r)
		return r;

	/* from here on, a failure undoes what was done */
	g_cam_open = 1;
	r = SAMPLE_COMM_VI_StartSensor(&g_vi);
	if (!r)
		r = SAMPLE_COMM_VI_StartDev(&g_vi.astViInfo[0]);
	if (!r)
		r = SAMPLE_COMM_VI_StartMIPI(&g_vi);
	if (!r)
		r = SAMPLE_COMM_VI_SensorProbe(&g_vi);
	if (r) {
		rc_cam_close();
		return r;
	}

	memset(&pipe, 0, sizeof(pipe));
	pipe.bYuvSkip = CVI_FALSE;
	pipe.u32MaxW = size.u32Width;
	pipe.u32MaxH = size.u32Height;
	pipe.enPixFmt = PIXEL_FORMAT_RGB_BAYER_12BPP;
	pipe.enBitWidth = DATA_BITWIDTH_12;
	pipe.stFrameRate.s32SrcFrameRate = -1;
	pipe.stFrameRate.s32DstFrameRate = -1;
	pipe.bNrEn = CVI_TRUE;
	pipe.bYuvBypassPath = SAMPLE_COMM_VI_GetYuvBypassSts(g_ini.enSnsType[0]);
	pipe.enCompressMode = g_vi.astViInfo[0].stChnInfo.enCompressMode;
	r = CVI_VI_CreatePipe(0, &pipe);
	if (!r)
		r = CVI_VI_StartPipe(0);
	if (!r)
		r = SAMPLE_COMM_VI_CreateIsp(&g_vi);
	if (!r)
		r = SAMPLE_COMM_VI_StartViChn(&g_vi);
	if (r) {
		rc_cam_close();
		return r;
	}
	pin_vi_last();
	return 0;
}

void rc_cam_close(void)
{
	if (!g_cam_open)
		return;
	SAMPLE_COMM_VI_DestroyIsp(&g_vi);
	SAMPLE_COMM_VI_DestroyVi(&g_vi);
	unpin_vi();
	g_sharp_saved = 0;
	g_cam_open = 0;
}

static CVI_U8 scale_gain(CVI_U8 g, unsigned int s)
{
	unsigned int v = (unsigned int)g * s / 50;

	return v > 255 ? 255 : (CVI_U8)v;
}

int rc_isp_image(unsigned int brightness, unsigned int contrast, unsigned int saturation,
		 unsigned int hue, unsigned int sharpness)
{
	ISP_CSC_ATTR_S csc;
	ISP_SHARPEN_ATTR_S sh;
	CVI_S32 r;

	if (!g_cam_open)
		return CVI_FAILURE;
	r = CVI_ISP_GetCSCAttr(0, &csc);
	if (r)
		return r;
	csc.Enable = CVI_TRUE;
	csc.Luma = brightness > 100 ? 100 : brightness;
	csc.Contrast = contrast > 100 ? 100 : contrast;
	csc.Saturation = saturation > 100 ? 100 : saturation;
	csc.Hue = hue > 100 ? 100 : hue;
	r = CVI_ISP_SetCSCAttr(0, &csc);
	if (r)
		return r;

	r = CVI_ISP_GetSharpenAttr(0, &sh);
	if (r)
		return r;
	if (!g_sharp_saved) {
		memcpy(g_sharp_auto, sh.stAuto.GlobalGain, sizeof(g_sharp_auto));
		g_sharp_manual = sh.stManual.GlobalGain;
		g_sharp_saved = 1;
	}
	if (sharpness > 100)
		sharpness = 100;
	for (int i = 0; i < ISP_AUTO_ISO_STRENGTH_NUM; i++)
		sh.stAuto.GlobalGain[i] = scale_gain(g_sharp_auto[i], sharpness);
	sh.stManual.GlobalGain = scale_gain(g_sharp_manual, sharpness);
	return CVI_ISP_SetSharpenAttr(0, &sh);
}

int rc_cam_flip(int mirror, int flip)
{
	if (!g_cam_open)
		return CVI_FAILURE;
	return CVI_VI_SetChnFlipMirror(0, 0, flip ? CVI_TRUE : CVI_FALSE, mirror ? CVI_TRUE : CVI_FALSE);
}

/* encoder channels opened here (closed by rc_venc_close) */
static unsigned int g_venc_open;

int rc_venc_open(int chn, int codec, unsigned int width, unsigned int height,
		 unsigned int param, unsigned int gop)
{
	VENC_CHN_ATTR_S a;
	VENC_RECV_PIC_PARAM_S recv = { .s32RecvPicNum = -1 };
	CVI_S32 r;

	if (chn < 0 || chn >= 32 || (codec != RC_CODEC_JPEG && codec != RC_CODEC_H264))
		return CVI_FAILURE;
	memset(&a, 0, sizeof(a));
	a.stVencAttr.enType = codec == RC_CODEC_JPEG ? PT_JPEG : PT_H264;
	a.stVencAttr.u32MaxPicWidth = width;
	a.stVencAttr.u32MaxPicHeight = height;
	a.stVencAttr.u32PicWidth = width;
	a.stVencAttr.u32PicHeight = height;
	a.stVencAttr.bByFrame = CVI_TRUE;
	/* bitstream buffer: 0 means the vendor default (JPEG: 9 MiB, sized for
	 * 4096x2304), which does not fit the ION heap next to VI/VPSS; the
	 * failed allocation then hung the SoC (F7, 19/10/2026). 1 MiB is ~5x
	 * a 720p Q85 JPEG and ~4 s of H.264 at 2 Mbit/s. */
	a.stVencAttr.u32BufSize = 1024 * 1024;
	a.stGopAttr.enGopMode = VENC_GOPMODE_NORMALP;
	if (codec == RC_CODEC_JPEG) {
		a.stVencAttr.stAttrJpege.enReceiveMode = VENC_PIC_RECEIVE_SINGLE;
	} else {
		VENC_H264_CBR_S *cbr = &a.stRcAttr.stH264Cbr;

		/* Baseline: the H.264 profile every WebRTC browser decodes */
		a.stVencAttr.u32Profile = H264E_PROFILE_BASELINE;
		a.stRcAttr.enRcMode = VENC_RC_MODE_H264CBR;
		cbr->u32Gop = gop ? gop : 30;
		cbr->u32StatTime = 2;
		cbr->u32SrcFrameRate = 30;
		cbr->fr32DstFrameRate = 30;
		cbr->u32BitRate = param;
		a.stGopAttr.stNormalP.s32IPQpDelta = 2;
	}
	r = CVI_VENC_CreateChn(chn, &a);
	if (r)
		return r;
	g_venc_open |= 1u << chn;

	if (codec == RC_CODEC_H264) {
		/* The SPS profile follows the entropy coder (CABAC -> Main), and
		 * the encoder starts with CABAC whatever u32Profile says (F7). */
		VENC_H264_ENTROPY_S e = {
			.u32EntropyEncModeI = H264E_ENTROPY_CAVLC,
			.u32EntropyEncModeP = H264E_ENTROPY_CAVLC,
		};

		r = CVI_VENC_SetH264Entropy(chn, &e);
	} else {
		VENC_JPEG_PARAM_S jp;

		r = CVI_VENC_GetJpegParam(chn, &jp);
		if (!r) {
			jp.u32Qfactor = param ? (param > 99 ? 99 : param) : 80;
			r = CVI_VENC_SetJpegParam(chn, &jp);
		}
	}
	if (!r)
		r = CVI_VENC_StartRecvFrame(chn, &recv);
	if (r)
		rc_venc_close(chn);
	return r;
}

void rc_venc_close(int chn)
{
	if (chn < 0 || chn >= 32 || !(g_venc_open & (1u << chn)))
		return;
	CVI_VENC_StopRecvFrame(chn);
	CVI_VENC_ResetChn(chn);
	CVI_VENC_DestroyChn(chn);
	g_venc_open &= ~(1u << chn);
}

int rc_venc_request_idr(int chn)
{
	return CVI_VENC_RequestIDR(chn, CVI_TRUE);
}

int rc_venc_encode(int chn, const void *frame, int timeout_ms, unsigned char *buf,
		   unsigned int cap, unsigned int *len)
{
	VENC_PACK_S packs[16];
	VENC_CHN_STATUS_S st;
	VENC_STREAM_S s;
	unsigned int total = 0;
	CVI_S32 r;

	*len = 0;
	r = CVI_VENC_SendFrame(chn, (const VIDEO_FRAME_INFO_S *)frame, timeout_ms);
	if (r)
		return r;
	r = CVI_VENC_QueryStatus(chn, &st);
	if (r)
		return r;
	if (!st.u32CurPacks || st.u32CurPacks > 16)
		return CVI_FAILURE;
	memset(&s, 0, sizeof(s));
	memset(packs, 0, sizeof(packs));
	s.pstPack = packs;
	r = CVI_VENC_GetStream(chn, &s, timeout_ms);
	if (r)
		return r;
	for (CVI_U32 i = 0; i < s.u32PackCount; i++)
		total += packs[i].u32Len - packs[i].u32Offset;
	*len = total;
	if (total <= cap) {
		total = 0;
		for (CVI_U32 i = 0; i < s.u32PackCount; i++) {
			unsigned int n = packs[i].u32Len - packs[i].u32Offset;

			memcpy(buf + total, packs[i].pu8Addr + packs[i].u32Offset, n);
			total += n;
		}
	}
	CVI_VENC_ReleaseStream(chn, &s);
	return *len <= cap ? 0 : CVI_FAILURE;
}
