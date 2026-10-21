/* SPDX-License-Identifier: GPL-2.0 */
/* recamera-yolo -- F7 step 9 check, see recamera-yolo.c */
#ifndef RECAMERA_YOLO_H
#define RECAMERA_YOLO_H

#include <stdint.h>

struct yolo_det {
	float x1, y1, x2, y2;	/* model input pixels (letterboxed) */
	float score;
	int cls;
};

int yolo_open(const char *model, int *in_w, int *in_h);
void yolo_close(void);
int yolo_run(uint64_t paddr, int target, float thr, float nms,
	     struct yolo_det *out, int max_out);

#endif
