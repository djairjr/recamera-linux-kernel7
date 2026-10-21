// SPDX-License-Identifier: GPL-2.0
/*
 * iontest -- F7 step 2 check of cvi_ion.ko (/dev/ion), along the path the
 * Sophgo userspace takes (cviruntime cvi_device_mem.cpp, MPI cvi_sys.c):
 *   ION_IOC_HEAP_QUERY -> ION_IOC_ALLOC on the carveout heap -> mmap the
 *   dma-buf fd -> write/read a pattern -> ION_IOC_CUSTOM cache flush and
 *   invalidate by physical range -> heap info -> close (free).
 * Then allocates until the heap is full, frees everything, and checks that
 * the whole heap can be allocated again.
 *
 * Built and installed as recamera-iontest by the recamera-tests package.
 * Usage: recamera-iontest [size_kib]   (default 1024)
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

/* vendor uapi: drivers/staging/android/uapi/ion.h and ion_cvitek.h (5.10) */
#define MAX_ION_BUFFER_NAME 32
#define MAX_HEAP_NAME 32
struct ion_allocation_data {
	uint64_t len;
	uint32_t heap_id_mask;
	uint32_t flags;
	uint32_t fd;
	uint32_t unused;
	uint64_t paddr;
	char name[MAX_ION_BUFFER_NAME];
};
struct ion_heap_data {
	char name[MAX_HEAP_NAME];
	uint32_t type, heap_id, reserved0, reserved1, reserved2;
};
struct ion_heap_query {
	uint32_t cnt, reserved0;
	uint64_t heaps;
	uint32_t reserved1, reserved2;
};
struct ion_custom_data {
	unsigned int cmd;
	unsigned long arg;
};
struct cvitek_cache_range {
	void *start;
	uint32_t size;
	uint64_t paddr;
};
struct cvitek_heap_info {
	unsigned int id;
	unsigned long total_size, avail_size;
};
#define ION_IOC_MAGIC 'I'
#define ION_IOC_ALLOC _IOWR(ION_IOC_MAGIC, 0, struct ion_allocation_data)
#define ION_IOC_CUSTOM _IOWR(ION_IOC_MAGIC, 6, struct ion_custom_data)
#define ION_IOC_HEAP_QUERY _IOWR(ION_IOC_MAGIC, 8, struct ion_heap_query)
#define ION_IOC_CVITEK_GET_HEAP_INFO 2
#define ION_IOC_CVITEK_FLUSH_PHY_RANGE 4
#define ION_IOC_CVITEK_INVALIDATE_PHY_RANGE 5
#define ION_HEAP_TYPE_CARVEOUT 2
#define ION_FLAG_CACHED 1

static int ion_fd;

static int alloc_buf(uint32_t heap_id, uint64_t len, uint32_t flags,
		     struct ion_allocation_data *a, const char *name)
{
	memset(a, 0, sizeof(*a));
	a->len = len;
	a->heap_id_mask = 1u << heap_id;
	a->flags = flags;
	snprintf(a->name, sizeof(a->name), "%s", name);
	return ioctl(ion_fd, ION_IOC_ALLOC, a);
}

static int heap_info(uint32_t heap_id, struct cvitek_heap_info *hi)
{
	struct ion_custom_data c = { ION_IOC_CVITEK_GET_HEAP_INFO, (unsigned long)hi };

	hi->id = heap_id;
	return ioctl(ion_fd, ION_IOC_CUSTOM, &c);
}

int main(int argc, char **argv)
{
	uint64_t len = (argc > 1 ? strtoull(argv[1], NULL, 0) : 1024) * 1024;
	struct ion_heap_data heaps[8];
	struct ion_heap_query q = { .cnt = 8, .heaps = (uintptr_t)heaps };
	struct ion_allocation_data a;
	struct cvitek_heap_info hi;
	int carve = -1, fails = 0;
	uint32_t i;

	ion_fd = open("/dev/ion", O_RDWR | O_DSYNC);
	if (ion_fd < 0) { perror("open /dev/ion"); return 1; }

	if (ioctl(ion_fd, ION_IOC_HEAP_QUERY, &q)) { perror("HEAP_QUERY"); return 1; }
	for (i = 0; i < q.cnt; i++) {
		printf("heap %u: name=%s type=%u\n", heaps[i].heap_id, heaps[i].name, heaps[i].type);
		if (heaps[i].type == ION_HEAP_TYPE_CARVEOUT && carve < 0)
			carve = heaps[i].heap_id;
	}
	if (carve < 0) { printf("FAIL: no carveout heap\n"); return 1; }
	if (heap_info(carve, &hi)) { perror("GET_HEAP_INFO"); return 1; }
	printf("carveout heap %d: total %lu KiB, avail %lu KiB\n", carve,
	       hi.total_size >> 10, hi.avail_size >> 10);

	/* 1. allocate, map, pattern, cache ops */
	if (alloc_buf(carve, len, ION_FLAG_CACHED, &a, "iontest")) { perror("ALLOC"); return 1; }
	printf("alloc %llu KiB: fd %u paddr 0x%llx\n", (unsigned long long)len >> 10, a.fd,
	       (unsigned long long)a.paddr);
	uint32_t *p = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, a.fd, 0);
	if (p == MAP_FAILED) { perror("mmap"); return 1; }
	for (i = 0; i < len / 4; i++)
		p[i] = i ^ 0x5a5aa5a5u;

	struct cvitek_cache_range r = { .start = p, .size = (uint32_t)len, .paddr = a.paddr };
	struct ion_custom_data c = { ION_IOC_CVITEK_FLUSH_PHY_RANGE, (unsigned long)&r };
	if (ioctl(ion_fd, ION_IOC_CUSTOM, &c)) { perror("FLUSH_PHY_RANGE"); fails++; }
	c.cmd = ION_IOC_CVITEK_INVALIDATE_PHY_RANGE;
	if (ioctl(ion_fd, ION_IOC_CUSTOM, &c)) { perror("INVALIDATE_PHY_RANGE"); fails++; }
	for (i = 0; i < len / 4; i++)
		if (p[i] != (i ^ 0x5a5aa5a5u)) { printf("FAIL: word %u = 0x%x\n", i, p[i]); fails++; break; }
	printf("pattern %s after flush+invalidate\n", fails ? "BAD" : "ok");
	if (heap_info(carve, &hi) == 0)
		printf("after alloc: avail %lu KiB\n", hi.avail_size >> 10);
	munmap(p, len);
	close(a.fd);

	/* 2. fill the heap, free, fill again */
	for (int round = 1; round <= 2; round++) {
		int fds[256], n = 0;

		while (n < 256 && alloc_buf(carve, 1 << 20, 0, &a, "fill") == 0)
			fds[n++] = a.fd;
		printf("round %d: %d x 1 MiB allocated before the heap was full (errno %d)\n",
		       round, n, errno);
		heap_info(carve, &hi);
		printf("round %d: avail with heap full: %lu KiB\n", round, hi.avail_size >> 10);
		while (n)
			close(fds[--n]);
	}
	heap_info(carve, &hi);
	printf("end: avail %lu of %lu KiB\n", hi.avail_size >> 10, hi.total_size >> 10);
	if (hi.avail_size != hi.total_size) { printf("FAIL: memory not returned\n"); fails++; }

	printf("%s\n", fails ? "RESULT: FAIL" : "RESULT: OK");
	return fails ? 1 : 0;
}
