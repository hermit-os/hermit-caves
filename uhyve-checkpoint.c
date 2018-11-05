/*
 * Copyright (c) 2018, RWTH Aachen University
 * Author(s): Stefan Lankes <slankes@eonerc.rwth-aachen.de>
 *            Simon Pickartz <spickartz@eonerc.rwth-aachen.de>
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
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
 * AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
 * IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
 * ARE DISCLAIMED. IN NO EVENT SHALL THE REGENTS OR CONTRIBUTORS BE LIABLE FOR
 * ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
 * SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
 * CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
 * OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
 * OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

#define _GNU_SOURCE

#define MAX_FNAME (256)

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/time.h>

#include "uhyve-checkpoint.h"
#include "uhyve.h"

extern pthread_barrier_t barrier;
extern uint32_t          ncores;
extern uint8_t *         guest_mem;
extern uint64_t          elf_entry;
extern uint32_t          no_checkpoint;
extern bool              full_checkpoint;
extern size_t            guest_size;
extern char *            guest_path;
extern int               kvm, vmfd, netfd, efd, mig_efd;
extern pthread_t *       vcpu_threads;
extern bool              verbose;

static FILE *chk_file = NULL;
static char *chk_path = NULL;

/**
 * \brief Opens the checkpoint file for writing
 *
 * \param path The path to the checkpoint directory
 */
static void
open_chk_file(char *path)
{
	char fname[MAX_FNAME];
	snprintf(fname, MAX_FNAME, "%s/chk%u_mem.dat", path, no_checkpoint);
	chk_file = fopen(fname, "w");
	if (chk_file == NULL) {
		err(1, "fopen: unable to open file");
	}
}

/**
 * \brief Closes the checkpoint file
 */
static void
close_chk_file(void)
{
	fclose(chk_file);
}

/**
 * \brief Writes a memory location to the checkpoint file
 *
 * \param addr The memory location to be written to the file
 * \param bytes The amount of bytes to be written
 */
static void
write_chk_file(void *addr, size_t bytes)
{
	if (fwrite(addr, bytes, 1, chk_file) != 1) {
		err(1, "fwrite failed");
	}
}

/**
 * \brief Writes a memory page to the checkpoint file
 *
 * \param entry A pointer to the page table entry
 * \param entry_size The size of the page table entry
 * \param page A pointer to the memory page
 * \param page_size The size of the memory page
 */
static void
write_mem_page_to_chk_file(void *entry, size_t entry_size, void *page,
			   size_t page_size)
{
	write_chk_file(entry, entry_size);
	write_chk_file(page, page_size);
}

/**
 * \brief Load the checkpoint configuration file
 *
 * \param chk_path Path to the checkpoint directory
 *
 * This function reads the checkpoint configuration from the 'chk_config.txt'
 * within the given checkpoint directory.
 *
 * TODO: use json instead of plain text file
 */
int32_t
load_checkpoint_config(const char *chk_path)
{
	int  tmp = 0;
	char chk_config_name[MAX_FNAME];
	snprintf(chk_config_name, MAX_FNAME, "%s/chk_config.txt", chk_path);
	FILE *f = fopen(chk_config_name, "r");

	if (f == NULL)
		return -1;

	guest_path = (char *)malloc(MAX_FNAME);
	fscanf(f, "application path: %s\n", guest_path);
	fscanf(f, "number of cores: %u\n", &ncores);
	fscanf(f, "memory size: 0x%zx\n", &guest_size);
	fscanf(f, "checkpoint number: %u\n", &no_checkpoint);
	fscanf(f, "entry point: 0x%zx", &elf_entry);
	fscanf(f, "full checkpoint: %d", &tmp);
	full_checkpoint = tmp ? true : false;

	fclose(f);

	return 0;
}

/**
 * \brief The checkpoint handler for the VCPUs
 *
 * This handler calls the architecture-specific method to store the VCPU state.
 */
void
vcpu_thread_chk_handler(int signum)
{
	pthread_barrier_wait(&barrier);
	write_cpu_state(chk_path);
	pthread_barrier_wait(&barrier);
}

/**
 * \brief Create a checkpoint
 *
 * \param path The directory where the checkpoint should be stored
 * \param full_checkpoint Make a complete dump of the guest memory
 *
 * This function actually creates a checkpoint and writes it to the hard disk.
 * TODO: determine checkpoint number from given directory
 */
void
create_checkpoint(char *path, bool full_checkpoint)
{
	struct stat    st = {0};
	struct timeval begin, end;

	if (verbose)
		gettimeofday(&begin, NULL);

	// create the checkpoint directory
	if (stat(path, &st) == -1)
		mkdir(path, 0700);

	// Request the VCPUs to write their CPU state
	chk_path = path;
	for (size_t i = 0; i < ncores; i++)
		if (vcpu_threads[i] != pthread_self())
			pthread_kill(vcpu_threads[i], SIGTHRCHKP);

	// Request the VCPUs to write their state to the hard disk
	pthread_barrier_wait(&barrier);
	write_cpu_state(chk_path);

	// Open the checkpoint file for the memor content
	open_chk_file(path);

	/*
	struct kvm_irqchip irqchip = {};
	if (cap_irqchip)
		kvm_ioctl(vmfd, KVM_GET_IRQCHIP, &irqchip);
	else
		memset(&irqchip, 0x00, sizeof(irqchip));
	if (fwrite(&irqchip, sizeof(irqchip), 1, f) != 1)
		err(1, "fwrite failed");
	*/

	// Write the guest clock
	struct kvm_clock_data clock = {};
	kvm_ioctl(vmfd, KVM_GET_CLOCK, &clock);
	write_chk_file(&clock, sizeof(clock));

	// This is the actual page walk
	determine_dirty_pages(write_mem_page_to_chk_file);

	// Close the checkpoint file and release VCPUs
	close_chk_file();
	pthread_barrier_wait(&barrier);

	// update configuration file
	char chk_config_name[MAX_FNAME];
	snprintf(chk_config_name, MAX_FNAME, "%s/chk_config.txt", chk_path);
	FILE *f = fopen(chk_config_name, "w");
	if (f == NULL) {
		err(1, "fopen: unable to open file");
	}

	fprintf(f, "application path: %s\n", guest_path);
	fprintf(f, "number of cores: %u\n", ncores);
	fprintf(f, "memory size: 0x%zx\n", guest_size);
	fprintf(f, "checkpoint number: %u\n", no_checkpoint);
	fprintf(f, "entry point: 0x%zx\n", elf_entry);
	if (full_checkpoint)
		fprintf(f, "full checkpoint: 1");
	else
		fprintf(f, "full checkpoint: 0");

	fclose(f);

	if (verbose) {
		gettimeofday(&end, NULL);
		size_t msec = (end.tv_sec - begin.tv_sec) * 1000;
		msec += (end.tv_usec - begin.tv_usec) / 1000;
		fprintf(stderr,
			"Create checkpoint %u in %zd ms\n",
			no_checkpoint,
			msec);
	}

	no_checkpoint++;
}

/**
 * \brief Restore a checkpoint
 *
 * \param path The directory where the checkpoint is stored
 *
 * This function restores from a given checkpoint.
 */
int32_t
restore_checkpoint(char *path)
{
	char           fname[MAX_FNAME];
	size_t         location;
	size_t         paddr = elf_entry;
	int            ret;
	struct timeval begin, end;
	uint32_t       i;

	if (verbose)
		gettimeofday(&begin, NULL);

	i = full_checkpoint ? no_checkpoint : 0;
	for (; i <= no_checkpoint; i++) {
		snprintf(fname, MAX_FNAME, "%s/chk%u_mem.dat", path, i);

		// Open the checkpoint file for reading
		FILE *f = fopen(fname, "r");
		if (f == NULL)
			return -1;
		// Call the arch-specific restore routine
		if (load_checkpoint(f, (i == no_checkpoint)) < 0)
			return -1;

		// Close the checkpoint file
		fclose(f);
	}

	if (verbose) {
		gettimeofday(&end, NULL);
		size_t msec = (end.tv_sec - begin.tv_sec) * 1000;
		msec += (end.tv_usec - begin.tv_usec) / 1000;
		fprintf(stderr,
			"Load checkpoint %u in %zd ms\n",
			no_checkpoint,
			msec);
	}

	return 0;
}
