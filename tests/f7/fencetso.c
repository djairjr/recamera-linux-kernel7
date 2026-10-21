// SPDX-License-Identifier: GPL-2.0
/*
 * fencetso -- does this system survive a fence.tso? (F7 step 9)
 *
 * The SG2002's C906 traps on fence.tso; with the board fip's OpenSBI (fence.tso
 * emulated as fence rw,rw) the program runs to the end, without it it dies
 * with SIGILL (exit 132). Encoded by hand, so any compiler gives the test.
 *
 * Built and installed as recamera-fencetso by the recamera-tests package.
 */
#include <stdio.h>
#include <time.h>

int main(void)
{
	struct timespec a, b;
	const int n = 100000;

	clock_gettime(CLOCK_MONOTONIC, &a);
	for (int i = 0; i < n; i++)
		__asm__ volatile(".word 0x8330000f" ::: "memory"); /* fence.tso */
	clock_gettime(CLOCK_MONOTONIC, &b);
	printf("fence.tso x %d: OK, %.2f us each\n", n,
	       ((b.tv_sec - a.tv_sec) * 1e9 + (b.tv_nsec - a.tv_nsec)) / n / 1e3);
	return 0;
}
