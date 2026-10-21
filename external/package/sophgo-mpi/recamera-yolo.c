// SPDX-License-Identifier: GPL-2.0
/*
 * recamera-yolo -- F7 step 9 check: YOLO11/YOLOv8 detection on the SG2002 NPU
 * through libcviruntime, fed straight from a VPSS frame (no copy).
 *
 * Model input (read from the .cvimodel): images_raw, UINT8 [1,640,640,3],
 * RGB_PACKED, mean 0 / scale 1 (the /255 is inside the model), so the VPSS
 * channel delivers 640x640 PIXEL_FORMAT_RGB_888, letterboxed with grey 114.
 * Model output: per stride 8/16/32 one box tensor [1,64,H,W] (4 sides x 16
 * DFL bins) and one class tensor [1,C,H,W] (logits), INT8 with a qscale
 * (real = int8 * qscale), NCHW. Decoding, sigmoid and NMS are ours.
 */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cviruntime.h"
#include "recamera-yolo.h"

#define YOLO_REG_MAX	16
#define YOLO_MAX_DET	300
#define YOLO_MAX_SCALES	3

struct yolo_scale {
	CVI_TENSOR *box, *cls;
	int h, w, stride;
	float box_q, cls_q;
};

static CVI_MODEL_HANDLE g_model;
static CVI_TENSOR *g_in, *g_out;
static int32_t g_nin, g_nout;
static struct yolo_scale g_sc[YOLO_MAX_SCALES];
static int g_nsc, g_ncls, g_in_w, g_in_h;

int yolo_open(const char *model, int *in_w, int *in_h)
{
	CVI_SHAPE s;
	int i;

	if (CVI_NN_RegisterModel(model, &g_model)) {
		printf("yolo: cannot load %s\n", model);
		return -1;
	}
	if (CVI_NN_GetInputOutputTensors(g_model, &g_in, &g_nin, &g_out, &g_nout) || g_nin != 1) {
		printf("yolo: bad tensors\n");
		return -1;
	}
	s = CVI_NN_TensorShape(&g_in[0]);
	g_in_h = s.dim[1];
	g_in_w = s.dim[2];
	printf("yolo: %s input %s %dx%dx%d, %d outputs\n", model, CVI_NN_TensorName(&g_in[0]),
	       (int)s.dim[1], (int)s.dim[2], (int)s.dim[3], g_nout);

	/* pair box (64 ch) and class tensors by their grid size */
	for (i = 0; i < g_nout; i++) {
		CVI_TENSOR *t = &g_out[i];
		int c, h, w, k;

		s = CVI_NN_TensorShape(t);
		c = s.dim[1];
		h = s.dim[2];
		w = s.dim[3];
		for (k = 0; k < g_nsc && g_sc[k].h != h; k++)
			;
		if (k == g_nsc) {
			if (g_nsc == YOLO_MAX_SCALES) {
				printf("yolo: too many output scales\n");
				return -1;
			}
			g_nsc++;
			g_sc[k].h = h;
			g_sc[k].w = w;
			g_sc[k].stride = g_in_h / h;
		}
		if (c == 4 * YOLO_REG_MAX) {
			g_sc[k].box = t;
			g_sc[k].box_q = CVI_NN_TensorQuantScale(t);
		} else {
			g_sc[k].cls = t;
			g_sc[k].cls_q = CVI_NN_TensorQuantScale(t);
			g_ncls = c;
		}
		printf("yolo:   out %d: %s [%d,%d,%d] qscale %f\n", i, CVI_NN_TensorName(t), c, h, w,
		       CVI_NN_TensorQuantScale(t));
	}
	for (i = 0; i < g_nsc; i++)
		if (!g_sc[i].box || !g_sc[i].cls) {
			printf("yolo: output layout is not YOLOv8/11 detection (box+cls per scale)\n");
			return -1;
		}
	*in_w = g_in_w;
	*in_h = g_in_h;
	return 0;
}

void yolo_close(void)
{
	if (g_model)
		CVI_NN_CleanupModel(g_model);
	g_model = NULL;
}

/* expected distance of one box side: softmax over the 16 DFL bins */
static float dfl(const int8_t *p, int plane, float q)
{
	float m = -1e30f, sum = 0, acc = 0, e;
	int i;

	for (i = 0; i < YOLO_REG_MAX; i++)
		if (p[i * plane] * q > m)
			m = p[i * plane] * q;
	for (i = 0; i < YOLO_REG_MAX; i++) {
		e = expf(p[i * plane] * q - m);
		sum += e;
		acc += e * i;
	}
	return acc / sum;
}

static float iou(const struct yolo_det *a, const struct yolo_det *b)
{
	float x1 = fmaxf(a->x1, b->x1), y1 = fmaxf(a->y1, b->y1);
	float x2 = fminf(a->x2, b->x2), y2 = fminf(a->y2, b->y2);
	float inter = fmaxf(0, x2 - x1) * fmaxf(0, y2 - y1);
	float ua = (a->x2 - a->x1) * (a->y2 - a->y1) + (b->x2 - b->x1) * (b->y2 - b->y1) - inter;

	return ua > 0 ? inter / ua : 0;
}

static int by_score(const void *a, const void *b)
{
	float d = ((const struct yolo_det *)b)->score - ((const struct yolo_det *)a)->score;

	return (d > 0) - (d < 0);
}

/*
 * One inference on the frame at physical address paddr (the model input
 * size, RGB packed). Detections of class target (-1: any class) with
 * score >= thr, after NMS, in input (letterboxed) pixels.
 */
int yolo_run(uint64_t paddr, int target, float thr, float nms,
	     struct yolo_det *out, int max_out)
{
	static struct yolo_det cand[YOLO_MAX_DET];
	/* class logit threshold: sigmoid(x) >= thr <=> x >= logit(thr) */
	float lthr = logf(thr / (1 - thr));
	int n = 0, kept = 0, s, i, j;

	if (CVI_NN_SetTensorPhysicalAddr(&g_in[0], paddr)) {
		printf("yolo: SetTensorPhysicalAddr failed\n");
		return -1;
	}
	if (CVI_NN_Forward(g_model, g_in, g_nin, g_out, g_nout)) {
		printf("yolo: Forward failed\n");
		return -1;
	}

	for (s = 0; s < g_nsc; s++) {
		struct yolo_scale *sc = &g_sc[s];
		const int8_t *box = CVI_NN_TensorPtr(sc->box);
		const int8_t *cls = CVI_NN_TensorPtr(sc->cls);
		int plane = sc->h * sc->w;

		for (i = 0; i < plane; i++) {
			int c0 = target >= 0 ? target : 0, c1 = target >= 0 ? target + 1 : g_ncls;
			int best = -1, c;
			int8_t bq = -128;
			float l;

			for (c = c0; c < c1; c++)
				if (cls[c * plane + i] > bq) {
					bq = cls[c * plane + i];
					best = c;
				}
			l = bq * sc->cls_q;
			if (best < 0 || l < lthr || n == YOLO_MAX_DET)
				continue;

			float gx = i % sc->w + 0.5f, gy = i / sc->w + 0.5f;
			float dl = dfl(box + 0 * YOLO_REG_MAX * plane + i, plane, sc->box_q);
			float dt = dfl(box + 1 * YOLO_REG_MAX * plane + i, plane, sc->box_q);
			float dr = dfl(box + 2 * YOLO_REG_MAX * plane + i, plane, sc->box_q);
			float db = dfl(box + 3 * YOLO_REG_MAX * plane + i, plane, sc->box_q);

			cand[n].x1 = (gx - dl) * sc->stride;
			cand[n].y1 = (gy - dt) * sc->stride;
			cand[n].x2 = (gx + dr) * sc->stride;
			cand[n].y2 = (gy + db) * sc->stride;
			cand[n].score = 1 / (1 + expf(-l));
			cand[n].cls = best;
			n++;
		}
	}

	/* greedy NMS per class */
	qsort(cand, n, sizeof(cand[0]), by_score);
	for (i = 0; i < n && kept < max_out; i++) {
		for (j = 0; j < kept; j++)
			if (out[j].cls == cand[i].cls && iou(&out[j], &cand[i]) > nms)
				break;
		if (j == kept)
			out[kept++] = cand[i];
	}
	return kept;
}
