// SPDX-License-Identifier: GPL-2.0
/*
 * recamera-ioninfo -- free/total memory of the ION heaps (cvi_ion.ko custom ioctl).
 *
 * After every MPI program exits, the carveout heap must be back to its total:
 * the VB pools and VI buffers live in the kernel and outlive a process that
 * dies without its teardown (F7 step 9).
 */
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>

struct ion_custom_data {
	unsigned int cmd;
	unsigned long arg;
};
struct cvitek_heap_info {
	unsigned int id;
	unsigned long total_size, avail_size;
};
#define ION_IOC_CUSTOM _IOWR('I', 6, struct ion_custom_data)
#define ION_IOC_CVITEK_GET_HEAP_INFO 2

int main(void)
{
	int fd = open("/dev/ion", O_RDWR);

	if (fd < 0) {
		perror("/dev/ion");
		return 1;
	}
	for (unsigned int id = 0; id < 4; id++) {
		struct cvitek_heap_info hi = { .id = id };
		struct ion_custom_data c = { ION_IOC_CVITEK_GET_HEAP_INFO, (unsigned long)&hi };

		if (ioctl(fd, ION_IOC_CUSTOM, &c) == 0)
			printf("heap %u: total %lu KiB avail %lu KiB\n", id,
			       hi.total_size >> 10, hi.avail_size >> 10);
	}
	return 0;
}
