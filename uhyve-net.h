#ifndef __UHYVE_NET_H__
#define __UHYVE_NET_H__

#include <linux/kvm.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <assert.h>
#include <errno.h>
#include <unistd.h>

#include <sys/select.h>
#include <sys/stat.h>

/* network interface */
#include <sys/socket.h>
#include <linux/if.h>
#include <linux/if_tun.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <err.h>

#define SHAREDQUEUE_START       0x80000
#define UHYVE_NET_MTU           1500
#define UHYVE_QUEUE_SIZE        8

#define SHAREDQUEUE_FLOOR(x)	((x) & !0x3f)
#define SHAREDQUEUE_CEIL(x)		(((x) + 0x3f) & ~0x3f)

extern int netfd;

typedef struct { volatile uint64_t counter; } atomic_uint64_t __attribute__ ((aligned (64)));

inline static uint64_t atomic_uint64_read(atomic_uint64_t *d) {
	return d->counter;
}

inline static int64_t atomic_uint64_inc(atomic_uint64_t* d) {
	uint64_t res = 1;
	__asm__ volatile("lock xaddq %0, %1" : "+r"(res), "+m"(d->counter) : : "memory", "cc");
	return ++res;
}

typedef struct queue_inner {
	uint16_t len;
	uint8_t data[UHYVE_NET_MTU+34];
} queue_inner_t;

typedef struct shared_queue {
	atomic_uint64_t read;
	atomic_uint64_t written;
	uint8_t reserved[64-8];
	queue_inner_t inner[UHYVE_QUEUE_SIZE];
} shared_queue_t;

int uhyve_net_init(const char *hermit_netif);
char* uhyve_get_mac(void);

#endif
