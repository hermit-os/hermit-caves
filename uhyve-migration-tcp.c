/*
 * Copyright (c) 2018, Simon Pickartz, RWTH Aachen University
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

#define _GNU_SOURCE

#include <stdbool.h>
#include <stdlib.h>
#include <arpa/inet.h>
#include <errno.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>


#include "uhyve-migration.h"
#include "uhyve.h"


#ifndef __RDMA_MIGRATION__

extern mem_mappings_t mem_mappings;

static mem_mappings_t mappings_to_be_transferred = {NULL, 0};

void precopy_phase(mem_mappings_t guest_mem, mem_mappings_t mem_mappings)
{
	/* TODO: implement live-migration */
	if (mig_params.type == MIG_TYPE_LIVE) {
		fprintf(stderr, "[WARNING] Live-migration currently not "
				"supported via TCP/IP. Fallback to "
				"cold-migration!\n");
		mappings_to_be_transferred = guest_mem;
	} else {
		mappings_to_be_transferred = mem_mappings;
	}

	return;
}

void stop_and_copy_phase(void)
{
	/* determine migration mode */
	switch (mig_params.mode) {
	case MIG_MODE_INCREMENTAL_DUMP:
		fprintf(stderr, "[WARNING] Incremental dumps currently not "
				"supported via TCP/IP. Fallback to complete "
				"dump!\n");
	case MIG_MODE_COMPLETE_DUMP: {
		size_t i = 0;
		for (i=0; i<mappings_to_be_transferred.count; ++i) {
			send_data(mappings_to_be_transferred.mem_chunks[i].ptr,
				  mappings_to_be_transferred.mem_chunks[i].size);
		}
		break;
	}
	default:
		fprintf(stderr, "ERROR: Unknown migration mode. Abort!\n");
		exit(EXIT_FAILURE);
	}

	fprintf(stderr, "Guest memory sent!\n");
}

void recv_guest_mem(mem_mappings_t mem_mappings)
{
	size_t i = 0;
	for (i=0; i<mem_mappings.count; ++i) {
		recv_data(mem_mappings.mem_chunks[i].ptr, mem_mappings.mem_chunks[i].size);
	}
	fprintf(stderr, "Guest memory received!\n");
}
#endif /* __RDMA_MIGRATION__ not defined */
