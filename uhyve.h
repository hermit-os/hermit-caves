/*
 * Copyright (c) 2018, Stefan Lankes, RWTH Aachen University
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *    * Redistributions of source code must retain the above copyright
 *      notice, this list of conditions and the following disclaimer.
 *    * Redistributions in binary form must reproduce the above copyright
 *      notice, this list of conditions and the following disclaimer in the
 *      documentation and/or other materials provided with the distribution.
 *    * Neither the name of the University nor the names of its contributors
 *      may be used to endorse or promote products derived from this
 *      software without specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE REGENTS OR CONTRIBUTORS BE LIABLE FOR ANY
 * DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
 * (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
 * LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND
 * ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
 * SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

#ifndef __UHYVE_H__
#define __UHYVE_H__

#include <err.h>
#include <linux/kvm.h>

#define UHYVE_PORT_WRITE		0x400
#define UHYVE_PORT_OPEN			0x440
#define UHYVE_PORT_CLOSE		0x480
#define UHYVE_PORT_READ			0x500
#define UHYVE_PORT_EXIT			0x540
#define UHYVE_PORT_LSEEK		0x580

// Networkports
#define UHYVE_PORT_NETINFO              0x600
#define UHYVE_PORT_NETWRITE             0x640
#define UHYVE_PORT_NETREAD              0x680
#define UHYVE_PORT_NETSTAT              0x700

#define UHYVE_PORT_FREELIST 		0x720

/* Ports and data structures for uhyve command line arguments and envp
 * forwarding */
#define UHYVE_PORT_CMDSIZE		0x740
#define UHYVE_PORT_CMDVAL		0x780

#define UHYVE_UART_PORT			0x800

#define UHYVE_IRQ_BASE            11
#define UHYVE_IRQ_NET             (UHYVE_IRQ_BASE+0)
#define UHYVE_IRQ_MIGRATION       (UHYVE_IRQ_BASE+1)

#define SIGTHRCHKP 	(SIGRTMIN+0)
#define SIGTHRMIG 	(SIGRTMIN+1)

#define kvm_ioctl(fd, cmd, arg) ({ \
        const int ret = ioctl(fd, cmd, arg); \
        if(ret == -1) \
                err(1, "KVM: ioctl " #cmd " failed"); \
        ret; \
        })

#ifdef __x86_64__
#define MAX_MSR_ENTRIES 25
struct msr_data {
	struct kvm_msrs info;
	struct kvm_msr_entry entries[MAX_MSR_ENTRIES];
};

typedef struct _vcpu_state {
	struct msr_data msr_data;
	struct kvm_regs regs;
	struct kvm_sregs sregs;
	struct kvm_fpu fpu;
	struct kvm_lapic_state lapic;
	struct kvm_xsave xsave;
	struct kvm_xcrs xcrs;
	struct kvm_vcpu_events events;
	struct kvm_mp_state mp_state;
} vcpu_state_t;

#define typeof __typeof__
#define virt_to_phys_with_offset(virtual_address) ({ \
	size_t physical_address = 0; \
	size_t physical_address_end = 0; \
	virt_to_phys((size_t)virtual_address, &physical_address, &physical_address_end); \
	(typeof (virtual_address))(guest_mem+physical_address); \
	})
#else
typedef struct _vcpu_state {
	int dummy;
} vcpu_state_t;
#endif

/* see also: arch/<type>/mm/memory.c */
typedef struct free_list {
	size_t start, end;
	struct free_list* next;
	struct free_list* prev;
} free_list_t;

typedef struct _migration_metadata migration_metadata_t;

void print_registers(void);
void timer_handler(int signum);
void *migration_handler(void *arg);
void restore_cpu_state(vcpu_state_t cpu_state);
vcpu_state_t read_cpu_state(void);
vcpu_state_t save_cpu_state(void);
void write_cpu_state(void);
void init_cpu_state(uint64_t elf_entry);
int load_kernel(uint8_t* mem, char* path);
int load_checkpoint(uint8_t* mem, char* path);
int load_migration_data(uint8_t* mem);
void wait_for_incomming_migration(migration_metadata_t *metadata, uint16_t listen_portno);
void init_kvm_arch(void);
int load_kernel(uint8_t* mem, char* path);
size_t determine_dest_offset(size_t src_addr);
void determine_dirty_pages(void (*save_page_handler)(void*, size_t, void*, size_t));
void determine_mem_mappings(free_list_t *alloc_list);
void virt_to_phys(const size_t virtual_address, size_t* const physical_address, size_t* const physical_address_page_end);

#endif
