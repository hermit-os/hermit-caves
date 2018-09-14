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

#include <arpa/inet.h>
#include <errno.h>
#include <infiniband/verbs.h>
#include <infiniband/verbs_exp.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "uhyve-migration.h"
#include "uhyve.h"

#ifdef __RDMA_MIGRATION__

#define MIG_ITERS (4)

#define IB_CQ_ENTRIES (1)
#define IB_MAX_INLINE_DATA (0)
#define IB_MAX_DEST_RD_ATOMIC (1)
#define IB_MIN_RNR_TIMER (1)
#define IB_MAX_SEND_WR \
	(8192) // TODO: should be
		   // com_hndl.dev_attr_ex.orig_attr.max_qp_wr
		   // fix for mlx_5 adapter
#define IB_MAX_RECV_WR (1)
#define IB_MAX_SEND_SGE (1)
#define IB_MAX_RECV_SGE (1)

typedef enum ib_wr_ids {
	IB_WR_NO_ID = 0,
	IB_WR_WRITE_LAST_PAGE_ID,
	IB_WR_RECV_LAST_PAGE_ID,
	IB_WR_BASE_ID
} ib_wr_ids_t;

uint64_t cur_wr_id = IB_WR_BASE_ID;

typedef struct qp_info {
	uint32_t  qpn;
	uint16_t  lid;
	uint16_t  psn;
	uint32_t *keys;
	uint64_t  addr;
} qp_info_t;

typedef struct com_hndl {
	struct ibv_context *	  ctx;		   /* device context */
	struct ibv_device_attr_ex dev_attr_ex; /* extended device attributes */
	struct ibv_port_attr	  port_attr;   /* port attributes */
	struct ibv_pd *			  pd;		   /* protection domain */
	struct ibv_mr **		  mrs;		   /* memory regions */
	struct ibv_cq *			  cq;		   /* completion queue */
	struct ibv_qp *			  qp;		   /* queue pair */
	struct ibv_comp_channel * comp_chan;   /* comp. event channel */
	qp_info_t				  loc_qp_info;
	qp_info_t				  rem_qp_info;
	uint8_t					  used_port; /* port of the IB device */
	uint8_t *				  buf; /* the guest memory (with potential gaps!) */
	size_t					  mr_cnt; /* number of memory regions */
} com_hndl_t;

static com_hndl_t		   com_hndl;
static struct ibv_send_wr *send_list		= NULL;
static struct ibv_send_wr *send_list_last   = NULL;
static size_t			   send_list_length = 0;

/**
 * \brief Prints info of a send_wr
 *
 * \param id the ID of the send_wr
 */
static inline void print_send_wr_info(uint64_t id) {
	struct ibv_send_wr *search_wr = send_list;

	/* find send_wr with id */
	while (search_wr) {
		if (search_wr->wr_id == id) {
			fprintf(
				stderr,
				"[INFO] WR_ID: %llu; LADDR: 0x%llx; RADDR: 0x%llx; SIZE: %llu\n",
				search_wr->wr_id,
				search_wr->sg_list->addr,
				search_wr->wr.rdma.remote_addr,
				search_wr->sg_list->length);

			break;
		}

		search_wr = search_wr->next;
	}

	if (search_wr == NULL) {
		fprintf(stderr, "[ERROR] Could not find send_wr with ID %llu\n", id);
	}
}

/**
 * \brief Initializes the IB communication structures
 *
 * \param com_hndl the structure containing all communication relevant infos
 * \param buf the buffer that should be registrered with the QP
 *
 * This function sets up the IB communication channel. It registers the 'buf'
 * with a new protection domain. On its termination there is a QP in the INIT
 * state ready to be connected with the remote side.
 */
static void init_com_hndl(mem_mappings_t mem_mappings, bool sender) {
	/* initialize com_hndl */
	memset(&com_hndl, 0, sizeof(com_hndl));

	/* the guest physical memory is the communication buffer */
	com_hndl.buf	= guest_mem;
	com_hndl.mr_cnt = mem_mappings.count;

	struct ibv_device **device_list		  = NULL;
	int					num_devices		  = 0;
	bool				active_port_found = false;

	/* determine first available device */
	if ((device_list = ibv_get_device_list(&num_devices)) == NULL) {
		fprintf(stderr,
				"[ERROR] Could not determine available IB devices "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* find device with active port */
	size_t cur_dev = 0;
	for (cur_dev = 0; cur_dev < num_devices; ++cur_dev) {
		/* open the device context */
		if ((com_hndl.ctx = ibv_open_device(device_list[cur_dev])) == NULL) {
			fprintf(stderr,
					"[ERROR] Could not open the device context "
					"- %d (%s). Abort!\n",
					errno,
					strerror(errno));
			exit(EXIT_FAILURE);
		}

		/* query extended device capabilities (e.g., to check for ODP support */
		struct ibv_query_device_ex_input device_ex_input;
		if (ibv_query_device_ex(
				com_hndl.ctx, &device_ex_input, &com_hndl.dev_attr_ex) < 0) {
			fprintf(stderr,
					"[ERROR] Could not query extended device attributes "
					"- %d (%s). Abort!\n",
					errno,
					strerror(errno));
			exit(EXIT_FAILURE);
		}

		/* determine port count via normal device query (necessary for mlx_5) */
		if (ibv_query_device(com_hndl.ctx, &com_hndl.dev_attr_ex.orig_attr) <
			0) {
			fprintf(stderr,
					"[ERROR] Could not query normal device attributes "
					"- %d (%s). Abort!\n",
					errno,
					strerror(errno));
			exit(EXIT_FAILURE);
		}

		/* check all ports */
		size_t num_ports = com_hndl.dev_attr_ex.orig_attr.phys_port_cnt;
		for (size_t cur_port = 0; cur_port <= num_ports; ++cur_port) {
			/* query current port */
			if (ibv_query_port(com_hndl.ctx, cur_port, &com_hndl.port_attr) <
				0) {
				fprintf(stderr,
						"[ERROR] Could not query port %u "
						"- %d (%s). Abort!\n",
						cur_port,
						errno,
						strerror(errno));
				exit(EXIT_FAILURE);
			}

			if (com_hndl.port_attr.state == IBV_PORT_ACTIVE) {
				active_port_found  = 1;
				com_hndl.used_port = cur_port;
				break;
			}
		}

		/* close this device if no active port was found */
		if (!active_port_found) {
			if (ibv_close_device(com_hndl.ctx) < 0) {
				fprintf(stderr,
						"[ERROR] Could not close the device context "
						"- %d (%s). Abort!\n",
						errno,
						strerror(errno));
				exit(EXIT_FAILURE);
			}
		} else {
			break;
		}
	}

	if (!active_port_found) {
		fprintf(stderr, "[ERROR] No active port found. Abort!\n");
		exit(EXIT_FAILURE);
	}

	fprintf(stderr,
			"[INFO] Using device '%s' and port %u\n",
			ibv_get_device_name(device_list[cur_dev]),
			com_hndl.used_port);
	/* allocate protection domain */
	if ((com_hndl.pd = ibv_alloc_pd(com_hndl.ctx)) == NULL) {
		fprintf(stderr,
				"[ERROR] Could not allocate protection domain "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* register guest memory chunks with the protection domain */
	int i = 0;
	com_hndl.mrs =
		(struct ibv_mr **)malloc(sizeof(struct ibv_mr *) * com_hndl.mr_cnt);

	int access_flags = (IBV_ACCESS_LOCAL_WRITE | IBV_ACCESS_REMOTE_WRITE);
	// 	TODO: check ODP support via capability mask
	//	if ((mig_params.use_odp) &&
	//	    (com_hndl.dev_attr_ex.odp_caps.general_caps & IBV_ODP_SUPPORT) &&
	//	    (com_hndl.dev_attr_ex.odp_caps.per_transport_caps.rc_odp_caps &
	// IBV_ODP_SUPPORT_WRITE)) {
	if (mig_params.use_odp) {
		access_flags |= IBV_ACCESS_ON_DEMAND;
	}

	for (i = 0; i < com_hndl.mr_cnt; ++i) {
		/* register memory region */
		if ((com_hndl.mrs[i] = ibv_reg_mr(com_hndl.pd,
										  mem_mappings.mem_chunks[i].ptr,
										  mem_mappings.mem_chunks[i].size,
										  access_flags)) == NULL) {
			fprintf(
				stderr,
				"[ERROR] Could not register the memory region #%d (ptr: %llx; size: %llu) "
				"- %d (%s). Abort!\n",
				i,
				mem_mappings.mem_chunks[i].ptr,
				mem_mappings.mem_chunks[i].size,
				errno,
				strerror(errno));
			exit(EXIT_FAILURE);
		}

		fprintf(
			stderr,
			"[INFO] com_hndl.mrs[%d]->addr = 0x%llx; com_hndl->mrs[%d].length = %llu\n",
			i,
			com_hndl.mrs[i]->addr,
			i,
			com_hndl.mrs[i]->length);
	}

	/* create completion event channel */
	if ((com_hndl.comp_chan = ibv_create_comp_channel(com_hndl.ctx)) == NULL) {
		fprintf(stderr,
				"[ERROR] Could not create the completion channel "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* create the completion queue */
	if ((com_hndl.cq = ibv_create_cq(
			 com_hndl.ctx, IB_CQ_ENTRIES, NULL, com_hndl.comp_chan, 0)) ==
		NULL) {
		fprintf(stderr,
				"[ERROR] Could not create the completion queue "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* create send and recv queue pair  and initialize it */
	struct ibv_qp_init_attr init_attr = {
		.send_cq	= com_hndl.cq,
		.recv_cq	= com_hndl.cq,
		.cap		= {.max_send_wr		= IB_MAX_SEND_WR,
				   .max_recv_wr		= IB_MAX_RECV_WR,
				   .max_send_sge	= IB_MAX_SEND_SGE,
				   .max_recv_sge	= IB_MAX_RECV_SGE,
				   .max_inline_data = IB_MAX_INLINE_DATA},
		.qp_type	= IBV_QPT_RC,
		.sq_sig_all = 0 /* we do not want a CQE for each WR */
	};
	if ((com_hndl.qp = ibv_create_qp(com_hndl.pd, &init_attr)) == NULL) {
		fprintf(stderr,
				"[ERROR] Could not create the queue pair "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	struct ibv_qp_attr attr = {.qp_state		= IBV_QPS_INIT,
							   .pkey_index		= 0,
							   .port_num		= com_hndl.used_port,
							   .qp_access_flags = (IBV_ACCESS_REMOTE_WRITE)};
	if (ibv_modify_qp(com_hndl.qp,
					  &attr,
					  IBV_QP_STATE | IBV_QP_PKEY_INDEX | IBV_QP_PORT |
						  IBV_QP_ACCESS_FLAGS) < 0) {
		fprintf(stderr,
				"[ERROR] Could not set QP into init state "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* fill in local qp_info */
	com_hndl.loc_qp_info.qpn  = com_hndl.qp->qp_num;
	com_hndl.loc_qp_info.psn  = lrand48() & 0xffffff;
	com_hndl.loc_qp_info.addr = (uint64_t)com_hndl.buf;
	com_hndl.loc_qp_info.lid  = com_hndl.port_attr.lid;

	com_hndl.loc_qp_info.keys =
		(uint32_t *)malloc(sizeof(uint32_t) * com_hndl.mr_cnt);
	for (i = 0; i < com_hndl.mr_cnt; ++i) {
		com_hndl.loc_qp_info.keys[i] = com_hndl.mrs[i]->rkey;
	}
}

/**
 * \brief Frees IB related resources
 *
 * \param com_hndl the structure containing all communication relevant infos
 */
static void destroy_com_hndl(void) {
	if (ibv_destroy_qp(com_hndl.qp) < 0) {
		fprintf(stderr,
				"[ERROR] Could not destroy the queue pair "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	if (ibv_destroy_cq(com_hndl.cq) < 0) {
		fprintf(stderr,
				"[ERROR] Could not deallocate the protection domain "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	if (ibv_destroy_comp_channel(com_hndl.comp_chan) < 0) {
		fprintf(stderr,
				"[ERROR] Could not destroy the completion channel "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	int i = 0;
	for (i = 0; i < com_hndl.mr_cnt; ++i) {
		if (ibv_dereg_mr(com_hndl.mrs[i]) < 0) {
			fprintf(stderr,
					"[ERROR] Could not deregister MR #%d "
					"- %d (%s). Abort!\n",
					i,
					errno,
					strerror(errno));
			exit(EXIT_FAILURE);
		}
	}

	if (ibv_dealloc_pd(com_hndl.pd) < 0) {
		fprintf(stderr,
				"[ERROR] Could not deallocate the protection domain "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	if (ibv_close_device(com_hndl.ctx) < 0) {
		fprintf(stderr,
				"[ERROR] Could not close the device context "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* free dynamic data structures */
	free(com_hndl.loc_qp_info.keys);
	free(com_hndl.rem_qp_info.keys);
	free(com_hndl.mrs);

	com_hndl.loc_qp_info.keys = NULL;
	com_hndl.rem_qp_info.keys = NULL;
	com_hndl.mrs			  = NULL;
}

/**
 * \brief Connects the QP created within init_com_hndl
 *
 * \param com_hndl the structure containing all communication relevant infos
 *
 * This function performs the actual connection setup between the two QPs.
 */
static void con_com_buf(void) {
	/* transistion to ready-to-receive state */
	struct ibv_qp_attr qp_attr = {.qp_state	= IBV_QPS_RTR,
								  .path_mtu	= IBV_MTU_2048,
								  .dest_qp_num = com_hndl.rem_qp_info.qpn,
								  .rq_psn	  = com_hndl.rem_qp_info.psn,
								  .max_dest_rd_atomic = IB_MAX_DEST_RD_ATOMIC,
								  .min_rnr_timer	  = IB_MIN_RNR_TIMER,
								  .ah_attr			  = {
										 .is_global		= 0,
										 .sl			= 0,
										 .src_path_bits = 0,
										 .dlid			= com_hndl.rem_qp_info.lid,
										 .port_num		= com_hndl.used_port,
									 }};
	if (ibv_modify_qp(com_hndl.qp,
					  &qp_attr,
					  IBV_QP_STATE | IBV_QP_PATH_MTU | IBV_QP_DEST_QPN |
						  IBV_QP_RQ_PSN | IBV_QP_MAX_DEST_RD_ATOMIC |
						  IBV_QP_MIN_RNR_TIMER | IBV_QP_AV)) {
		fprintf(stderr,
				"[ERROR] Could not put QP into RTR state"
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(errno);
	}

	/* transistion to ready-to-send state */
	qp_attr.qp_state	  = IBV_QPS_RTS;
	qp_attr.timeout		  = 14;
	qp_attr.retry_cnt	 = 7;
	qp_attr.rnr_retry	 = 7; /* infinite retrys on RNR NACK */
	qp_attr.sq_psn		  = com_hndl.loc_qp_info.psn;
	qp_attr.max_rd_atomic = 1;
	if (ibv_modify_qp(com_hndl.qp,
					  &qp_attr,
					  IBV_QP_STATE | IBV_QP_TIMEOUT | IBV_QP_RETRY_CNT |
						  IBV_QP_RNR_RETRY | IBV_QP_SQ_PSN |
						  IBV_QP_MAX_QP_RD_ATOMIC)) {
		fprintf(stderr,
				"[ERROR] Could not put QP into RTS state"
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(errno);
	}
}

/**
 * \brief Returns the index of the MR enclosing a given mem_chunk
 */
static inline ssize_t determine_enclosing_mr(void *ptr) {
	size_t  i   = 0;
	ssize_t res = -1;
	for (i = 0; i < com_hndl.mr_cnt; ++i) {
		size_t cur_mr_start = (size_t)com_hndl.mrs[i]->addr;
		size_t cur_mr_end   = cur_mr_start + com_hndl.mrs[i]->length;
		if ((cur_mr_start <= (size_t)ptr) && (cur_mr_end > (size_t)ptr)) {
			res = i;
			break;
		}
	}

	return res;
}

/**
 * \brief Prefetches a given list of memory mappings
 */
static void prefetch_mem_mappings(mem_mappings_t mem_mappings) {
	int i, j, ret;
	for (i = 0; i < mem_mappings.count; ++i) {
		void * cur_ptr  = mem_mappings.mem_chunks[i].ptr;
		size_t cur_size = mem_mappings.mem_chunks[i].size;

		/* find enclosing MR
		 * -> assumption: a mapping with always fit within an MR
		 */
		ssize_t mr_num = 0;
		if (determine_enclosing_mr(cur_ptr) < 0) {
			fprintf(stderr,
					"[WARNING] Could not determine encloding MR "
					"for ptr: 0x%llx size: 0x%llx.\n",
					cur_ptr,
					cur_size);
			return;
		}

		/* prefetch the memory chunk */
		struct ibv_exp_prefetch_attr prefetch_attr = {
			.flags	 = IBV_EXP_PREFETCH_WRITE_ACCESS,
			.addr	  = cur_ptr,
			.length	= cur_size,
			.comp_mask = 0,
		};
		if ((ret = ibv_exp_prefetch_mr(com_hndl.mrs[mr_num], &prefetch_attr)) <
			0) {
			fprintf(stderr,
					"[WARNING] Could not prefetch within MR #%d - "
					"result %d - %d (%s).\n",
					i,
					ret,
					errno,
					strerror(errno));
		}
	}
}

/**
 * \brief Set the destination node for a migration
 *
 * \param ip_str a string containing the IPv4 addr of the destination
 * \param port the migration port
 */
static void exchange_qp_info(bool server) {
	size_t keys_size = sizeof(uint32_t) * com_hndl.mr_cnt;

	int res = 0;
	if (server) {
		/* general QP info */
		res = recv_data(&com_hndl.rem_qp_info, sizeof(qp_info_t));
		res = send_data(&com_hndl.loc_qp_info, sizeof(qp_info_t));

		/* remote keys */
		com_hndl.rem_qp_info.keys = (uint32_t *)malloc(keys_size);
		res = recv_data(com_hndl.rem_qp_info.keys, keys_size);
		res = send_data(com_hndl.loc_qp_info.keys, keys_size);
	} else {
		/* general QP info */
		res = send_data(&com_hndl.loc_qp_info, sizeof(qp_info_t));
		res = recv_data(&com_hndl.rem_qp_info, sizeof(qp_info_t));

		/* remote keys */
		com_hndl.rem_qp_info.keys = (uint32_t *)malloc(keys_size);
		res = send_data(com_hndl.loc_qp_info.keys, keys_size);
		res = recv_data(com_hndl.rem_qp_info.keys, keys_size);
	}

	fprintf(stderr,
			"[INFO] loc_qp_info (QPN: %lu; LID: %lu; PSN: %lu; ADDR: 0x%x ",
			com_hndl.loc_qp_info.qpn,
			com_hndl.loc_qp_info.lid,
			com_hndl.loc_qp_info.psn,
			com_hndl.loc_qp_info.addr);
	int i = 0;
	for (i = 0; i < com_hndl.mr_cnt; ++i) {
		fprintf(stderr, "KEY[%d]: %lu; ", i, com_hndl.loc_qp_info.keys[i]);
	}
	printf("\b\b)\n");

	fprintf(stderr,
			"[INFO] rem_qp_info (QPN: %lu; LID: %lu; PSN: %lu; ADDR: 0x%x ",
			com_hndl.rem_qp_info.qpn,
			com_hndl.rem_qp_info.lid,
			com_hndl.rem_qp_info.psn,
			com_hndl.rem_qp_info.addr);
	for (i = 0; i < com_hndl.mr_cnt; ++i) {
		fprintf(stderr, "KEY[%d]: %lu; ", i, com_hndl.rem_qp_info.keys[i]);
	}
	printf("\b\b)\n");
}

/**
 * \brief Prepares the an 'ibv_send_wr'
 *
 * This function prepares an 'ibv_send_wr' structure that is prepared for the
 * transmission of a single memory page using the IBV_WR_RDMA_WRITE verb.
 */
static inline struct ibv_send_wr *prepare_send_list_elem(void) {
	/* create work request */
	struct ibv_send_wr *send_wr =
		(struct ibv_send_wr *)calloc(1, sizeof(struct ibv_send_wr));
	struct ibv_sge *sge = (struct ibv_sge *)calloc(1, sizeof(struct ibv_sge));

	/* basic work request configuration */
	send_wr->next	= NULL;
	send_wr->sg_list = sge;
	send_wr->num_sge = 1;
	send_wr->wr_id   = ++cur_wr_id;
	send_wr->opcode  = IBV_WR_RDMA_WRITE;

	return send_wr;
}

/**
 * \brief Appends an 'ibv_send_wr' to the send_list
 *
 * \param send_wr WR to be appended to the send_list
 */
static inline void append_to_send_list(struct ibv_send_wr *send_wr) {
	if (send_list == NULL) {
		send_list = send_list_last = send_wr;
	} else {
		send_list_last->next = send_wr;
		send_list_last		 = send_list_last->next;
	}

	/* we have to request a CQE if max_send_wr is reached to avoid overflows */
	if ((++send_list_length % IB_MAX_SEND_WR) == 0) {
		send_list_last->send_flags = IBV_SEND_SIGNALED;
	}
}

/**
 * \brief Creates an 'ibv_send_wr' and appends it to the send_list
 *
 * \param addr the page table entry of the memory page
 * \param addr_size the size of the page table entry
 * \param page the buffer to be send in this WR
 * \param page_size the size of the buffer
 *
 * This function creates an 'ibv_send_wr' structure and appends this to the
 * global send_list. It sets the source/destination information and sets the
 * IBV_SEND_SIGNALED flag as appropriate.
 */
static void create_send_list_entry(void * addr,
								   size_t addr_size,
								   void * page,
								   size_t page_size) {
	/* create work request */
	struct ibv_send_wr *send_wr = prepare_send_list_elem();

	/* configure source buffer */
	int i = 0;
	for (i = 0; i < com_hndl.mr_cnt; ++i) {
		if (((uint64_t)page >= (uint64_t)com_hndl.mrs[i]->addr) &&
			((uint64_t)page < ((uint64_t)com_hndl.mrs[i]->addr +
							   (uint64_t)com_hndl.mrs[i]->length))) {
			send_wr->sg_list->addr   = (uintptr_t)page;
			send_wr->sg_list->length = page_size;
			send_wr->sg_list->lkey   = com_hndl.mrs[i]->lkey;

			send_wr->wr.rdma.rkey = com_hndl.rem_qp_info.keys[i];

			/* prefetch MR */
			if (mig_params.use_odp && mig_params.prefetch) {
				struct ibv_exp_prefetch_attr prefetch_attr = {
					.flags	 = IBV_EXP_PREFETCH_WRITE_ACCESS,
					.addr	  = page,
					.length	= page_size,
					.comp_mask = 0,
				};

				int ret = 0;
				if ((ret = ibv_exp_prefetch_mr(com_hndl.mrs[i],
											   &prefetch_attr)) < 0) {
					fprintf(
						stderr,
						"[WARNING] Could not prefetch within MR #%d - result %d "
						"- %d (%s).\n",
						i,
						ret,
						errno,
						strerror(errno));
				}
			}
			break;
		}
	}

	/* did we find the correct memory region? */
	if (i == com_hndl.mr_cnt) {
		fprintf(
			stderr,
			"[ERROR] Could not find a valid MR for address 0x%llx! (send_list_length = %llu)\n",
			page,
			send_list_length);
		return;
	}

	/* configure destination buffer */
	if (addr) {
		send_wr->wr.rdma.remote_addr =
			com_hndl.rem_qp_info.addr + determine_dest_offset(*(size_t *)addr);
	} else {
		send_wr->wr.rdma.remote_addr = com_hndl.rem_qp_info.addr;
	}

	/* apped work request to send list */
	append_to_send_list(send_wr);
}

/**
 * \brief Frees the send list
 */
static inline void cleanup_send_list(void) {
	struct ibv_send_wr *cur_send_wr = send_list;
	struct ibv_send_wr *tmp_send_wr = NULL;
	while (cur_send_wr != NULL) {
		free(cur_send_wr->sg_list);
		tmp_send_wr = cur_send_wr;
		cur_send_wr = cur_send_wr->next;
		free(tmp_send_wr);
	}
	send_list_length = 0;
}

/*
 * \brief Processes the send list by passing the send_wrs to the HCA
 */
static inline void process_send_list(void) {
	/* we have to call ibv_post_send() as long as 'send_list' contains elements
	 */
	struct ibv_wc		wc;
	struct ibv_send_wr *remaining_send_wr = NULL;
	do {
		/* send data */
		remaining_send_wr = NULL;
		if (ibv_post_send(com_hndl.qp, send_list, &remaining_send_wr) &&
			(errno != ENOMEM)) {
			fprintf(stderr,
					"[ERROR] Could not post send - %d (%s). Abort!\n",
					errno,
					strerror(errno));
			exit(EXIT_FAILURE);
		}

		/* wait for send WRs if CQ is full */
		int res = 0;
		do {
			if ((res = ibv_poll_cq(com_hndl.cq, 1, &wc)) < 0) {
				fprintf(stderr,
						"[ERROR] Could not poll on CQ - %d (%s). Abort!\n",
						errno,
						strerror(errno));
				exit(EXIT_FAILURE);
			}
		} while (res < 1);
		if (wc.status != IBV_WC_SUCCESS) {
			fprintf(stderr,
					"[ERROR] WR failed status %s (%d) for wr_id %llu\n",
					ibv_wc_status_str(wc.status),
					wc.status,
					wc.wr_id);

			print_send_wr_info(wc.wr_id);
		}
		send_list = remaining_send_wr;
	} while (remaining_send_wr);

	/* ensure that we receive the CQE for the last page */
	if (wc.wr_id != IB_WR_WRITE_LAST_PAGE_ID) {
		fprintf(stderr,
				"[ERROR] WR failed status %s (%d) for wr_id %d\n",
				ibv_wc_status_str(wc.status),
				wc.status,
				(int)wc.wr_id);
	}

	/* free send list */
	cleanup_send_list();
}

/**
 * \brief Prepares a send_list containing all memory defined by com_hndl.mrs
 *
 * This function creates as many send_wr items as required to cover all
 * com_hndl.mrs in accordance with the maximum message size that can be
 * transmitted per send_sr (com_hndl.port_attr.max_msg_sz).
 */
static inline void enqueue_all_mrs(void) {
	uint64_t max_msg_sz = com_hndl.port_attr.max_msg_sz;
	int		 i			= 0;

	/* send all MRs */
	for (i = 0; i < com_hndl.mr_cnt; ++i) {
		uint64_t cur_mr_length = com_hndl.mrs[i]->length;

		/* split the MR if it exceed the max_msg_sz */
		size_t cur_chunk = 0, max_chunks = cur_mr_length / max_msg_sz;
		for (cur_chunk; cur_chunk < max_chunks; ++cur_chunk) {
			size_t cur_offset	  = cur_chunk * max_msg_sz;
			size_t cur_glob_offset = cur_offset +
									 (uint64_t)com_hndl.mrs[i]->addr -
									 (uint64_t)guest_mem;
			create_send_list_entry(
				(void *)&cur_glob_offset,
				0,
				(void *)((uint64_t)com_hndl.mrs[i]->addr + cur_offset),
				max_msg_sz);
		}

		/* do we have a remainder? */
		uint64_t remainder = cur_mr_length % max_msg_sz;
		if (remainder) {
			size_t cur_offset	  = cur_mr_length - remainder;
			size_t cur_glob_offset = cur_offset +
									 (uint64_t)com_hndl.mrs[i]->addr -
									 (uint64_t)guest_mem;
			create_send_list_entry(
				(void *)&cur_glob_offset,
				0,
				(void *)((uint64_t)com_hndl.mrs[i]->addr + cur_offset),
				remainder);
		}
	}
}

/**
 * \brief A simple termination criterion for the live-migration
 */
static inline bool termination_criterion(void) {
	/* use a simple counter */
	static uint32_t mig_round = 0;

	return (mig_round++ == MIG_ITERS) ? true : false;
}

/**
 * \brief The pre-copy phase of the live-migration
 *
 * \param guest_mem the guest physical memory
 * \param mem_mappings the mapped memory regions
 *
 * This function initializes the IB connection and executes the precopy
 * iteration steps.
 */
void precopy_phase(mem_mappings_t guest_mem, mem_mappings_t mem_mappings) {
	/* the live migration needs the whole guest memory to be registered */
	if ((mig_params.type == MIG_TYPE_LIVE) || (mem_mappings.count == 0)) {
		init_com_hndl(guest_mem, true);
	} else {
		init_com_hndl(mem_mappings, true);
	}

	/* prefetch allocated regions if ODP is used */
	if (mig_params.use_odp && mig_params.prefetch) {
		prefetch_mem_mappings(mem_mappings);

		/* disable prefetching for cold/complete migration */
		if ((mig_params.mode == MIG_MODE_COMPLETE_DUMP) ||
			(mig_params.type == MIG_TYPE_COLD)) {
			mig_params.prefetch = false;
		}
	}

	/* establish the IB connection */
	exchange_qp_info(false);
	con_com_buf();

	/* perform pre-copy iterations */
	while (!(mig_params.type == MIG_TYPE_COLD) && !termination_criterion()) {
		/* iterate guest page tables
		 * -> ignore migration mode
		 * -> enforce INCREMENTAL dumps
		 */
		determine_dirty_pages(create_send_list_entry);

		/* is there anything to send? */
		if (send_list_length != 0) {
			/* we want a CQE for the last WR */
			send_list_last->wr_id	  = IB_WR_WRITE_LAST_PAGE_ID;
			send_list_last->send_flags = IBV_SEND_SIGNALED;

			process_send_list();
		} else {
			break;
		}
	}

	return;
}

/**
 * \brief The stop-and-copy phase of the live-migration
 *
 * This function performs the last step of the migration. After freezing the
 * VCPUs the guest-physical memory is transferred (another time) to the
 * destination.
 */
void stop_and_copy_phase(void) {
	int			res = 0, i = 0;
	static bool ib_initialized = false;

	/* determine migration mode */
	if (mig_params.type == MIG_TYPE_COLD) {
		switch (mig_params.mode) {
		case MIG_MODE_COMPLETE_DUMP:
			enqueue_all_mrs();
			break;
		case MIG_MODE_INCREMENTAL_DUMP:
			/* iterate guest page tables */
			determine_dirty_pages(create_send_list_entry);
			break;
		default:
			fprintf(stderr, "[ERROR] Unknown migration mode. Abort!\n");
			exit(EXIT_FAILURE);
		}
	} else if (mig_params.type == MIG_TYPE_LIVE) {
		determine_dirty_pages(create_send_list_entry);
	} else {
		fprintf(stderr, "[ERROR] Unknown migration type. Abort!\n");
		exit(EXIT_FAILURE);
	}

	/* create a dumy WR request if there is nothing to be sent */
	if (send_list_length == 0) {
		struct ibv_send_wr *send_wr = prepare_send_list_elem();
		append_to_send_list(send_wr);
	}

	/* we have to wait for the last WR before informing dest */
	send_list_last->wr_id	  = IB_WR_WRITE_LAST_PAGE_ID;
	send_list_last->opcode	 = IBV_WR_RDMA_WRITE_WITH_IMM;
	send_list_last->send_flags = IBV_SEND_SIGNALED | IBV_SEND_SOLICITED;
	send_list_last->imm_data   = htonl(0x1);

	process_send_list();

	/* free IB-related resources */
	destroy_com_hndl();
	ib_initialized = false;

	fprintf(stderr, "Guest memory sent!\n");
}

/**
 * \brief Receives the guest memory from the source
 *
 * The receive participates in the IB connection setup and waits for the
 * 'solicited' event sent with the last WR issued by the sender.
 *
 * \param mem_mappings the memory regions regions that have to be mapped
 */
void recv_guest_mem(mem_mappings_t mem_mappings) {
	int res = 0;

	/* prepare IB channel */
	init_com_hndl(mem_mappings, false);
	exchange_qp_info(true);
	con_com_buf();

	/* request notification on the event channel */
	if (ibv_req_notify_cq(com_hndl.cq, 1) < 0) {
		fprintf(stderr,
				"[ERROR] Could request notify for completion queue "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* post recv matching IBV_RDMA_WRITE_WITH_IMM */
	struct ibv_cq *		ev_cq;
	void *				ev_ctx;
	struct ibv_sge		sg;
	struct ibv_recv_wr  recv_wr;
	struct ibv_recv_wr *bad_wr;
	uint32_t			recv_buf = 0;

	memset(&sg, 0, sizeof(sg));
	sg.addr   = (uintptr_t)&recv_buf;
	sg.length = sizeof(recv_buf);
	sg.lkey   = com_hndl.mrs[0]->lkey;

	memset(&recv_wr, 0, sizeof(recv_wr));
	recv_wr.wr_id   = 0;
	recv_wr.sg_list = &sg;
	recv_wr.num_sge = 1;

	if (ibv_post_recv(com_hndl.qp, &recv_wr, &bad_wr) < 0) {
		fprintf(stderr,
				"[ERROR] Could post recv - %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* wait for requested event */
	if (ibv_get_cq_event(com_hndl.comp_chan, &ev_cq, &ev_ctx) < 0) {
		fprintf(stderr,
				"[ERROR] Could get event from completion channel "
				"- %d (%s). Abort!\n",
				errno,
				strerror(errno));
		exit(EXIT_FAILURE);
	}

	/* acknowledge the event */
	ibv_ack_cq_events(com_hndl.cq, 1);

	/* free IB-related resources */
	destroy_com_hndl();

	fprintf(stderr, "Guest memory received!\n");
}
#endif /* __RDMA_MIGRATION__ */
