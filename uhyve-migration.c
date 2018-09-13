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

#ifdef __x86_64__
#define _GNU_SOURCE

#include <arpa/inet.h>
#include <errno.h>
#include <semaphore.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "uhyve-migration.h"
#include "uhyve.h"

static struct sockaddr_in mig_server;
static int com_sock = 0;
static int listen_sock = 0;

extern sem_t mig_sem;
extern int mig_efd;

mig_params_t mig_params = {
	.type = MIG_TYPE_COLD,
	.mode = MIG_MODE_COMPLETE_DUMP,
	.use_odp = false,
	.prefetch = false,
};

extern mem_mappings_t mem_mappings;

/**
 * \brief Generates a setter for a migration parameter
 *
 * \param param The parameter that is to be set
 *
 * \param mig_param_str A string defining the respective migration parameter
 *
 */
#define define_migration_param_setter(param)                                   \
void                                                                           \
set_migration_##param(const char *mig_param_str)                               \
{                                                                              \
	if (mig_param_str == NULL)                                             \
		return;                                                        \
                                                                               \
	int i;                                                                 \
	bool found_param = false;                                              \
	for (i=0; i<sizeof(mig_##param##_conv)/sizeof(mig_##param##_conv[0]); ++i) { \
		if (!strcmp (mig_param_str, mig_##param##_conv[i].str)) {      \
			mig_params.param = mig_##param##_conv[i].mig_##param;\
			found_param = true;                                    \
		}                                                              \
	}                                                                      \
                                                                               \
	if (!found_param) {                                                    \
		/* we do not know this migration param */                      \
		fprintf(stderr, "[ERROR] Migration ##param## '%s' not "        \
				"supported. Fallback to default\n",            \
				mig_param_str);                                \
	}                                                                      \
	return;                                                                \
} \

#define define_migration_param_getter(param)                                   \
const char *                                                                         \
get_migration_##param##_str(mig_##param##_t mig_##param)                       \
{                                                                              \
	return mig_##param##_conv[mig_##param].str;                            \
} \

/* define setter for migration parameters */
define_migration_param_setter(type)
define_migration_param_setter(mode)
define_migration_param_getter(type)
define_migration_param_getter(mode)

/**
 * \brief prints the migration parameters
 */
void
print_migration_params(void)
{
	printf("========== MIGRATION PARAMETERS ==========\n");
	printf("   MODE     : %s\n", get_migration_mode_str(mig_params.mode));
	printf("   TYPE     : %s\n", get_migration_type_str(mig_params.type));
	printf("   USE ODP  : %u\n", mig_params.use_odp);
	printf("   PREFETCH : %u\n", mig_params.prefetch);
	printf("==========================================\n");
}

/**
 * \brief Sets the migration parameters in accordance with a given file
 *
 * \param mig_param_file path to the file containing the migration parameters
 */
void
set_migration_params(const char *mig_param_filename)
{
	if (mig_param_filename == NULL)
		return;

	FILE *mig_param_file = fopen(mig_param_filename, "r");
	char tmp_str[MAX_PARAM_STR_LEN];
	fscanf(mig_param_file, "mode: %s\n", tmp_str);
	set_migration_mode(tmp_str);
	fscanf(mig_param_file, "type: %s\n", tmp_str);
	set_migration_type(tmp_str);
	fscanf(mig_param_file, "use-odp: %u\n", (uint32_t*)&mig_params.use_odp);
	fscanf(mig_param_file, "prefetch: %u\n", (uint32_t*)&mig_params.prefetch);
}

/**
 * \brief Returns the configured migration type
 */
mig_type_t
get_migration_type(void)
{
	return mig_params.type;
}

/**
 * \brief Closes a socket
 *
 * \param sock the socket to be closed
 */
static inline void
close_sock(int sock)
{
	if (close(sock) < 0) {
		fprintf(stderr,
		    	"ERROR: Could not close the communication socket "
			"- %d (%s). Abort!\n",
			errno,
			strerror(errno));
		exit(EXIT_FAILURE);
	}
}

/**
 * \brief Set the destination node for a migration
 *
 * \param ip_str a string containing the IPv4 addr of the destination
 * \param port the migration port
 */
void set_migration_target(const char *ip_str, int port)
{
	/* determine server address */
	memset(&mig_server, '0', sizeof(mig_server));
	mig_server.sin_family = AF_INET;
	mig_server.sin_port = htons(port);

	int res = inet_pton(AF_INET, ip_str, &mig_server.sin_addr);
	if (res == 0) {
		fprintf(stderr, "'%s' is not a valid server address\n", ip_str);
	} else if (res < 0) {
		fprintf(stderr, "An error occured while retrieving the migration server address\n");
		perror("inet_pton");
	}
}

/**
 * \brief Connects to a migration target via TCP/IP
 */
int connect_to_server(void)
{
	int res = 0;
	char buf[INET_ADDRSTRLEN];
	if (inet_ntop(AF_INET, (const void*)&mig_server.sin_addr, buf, INET_ADDRSTRLEN) == NULL) {
		perror("inet_ntop");
		return -1;
	}

	if((com_sock = socket(AF_INET, SOCK_STREAM, 0)) < 0) {
		perror("socket");
		return -1;
	}

	fprintf(stderr, "[INFO] Trying to connect to migration server: %s\n", buf);
	if (connect(com_sock, (struct sockaddr *)&mig_server, sizeof(mig_server)) < 0) {
		perror("connect");
		return -1;
    	}
	fprintf(stderr, "[INFO] Successfully connected to: %s\n", buf);

	/* send migration parameters */
	res = send_data(&mig_params, sizeof(mig_params_t));
	print_migration_params();

	return 0;
}


/**
 * \brief Waits for a migration source to connect via TCP/IP
 *
 * \param listen_portno the port of the migration socket
 */
void wait_for_client(uint16_t listen_portno)
{
	int client_addr_len = 0, res = 0;
	struct sockaddr_in serv_addr;
	struct sockaddr_in client_addr;

	/* open migration socket */
	fprintf(stderr, "[INFO] Waiting for incomming migration request ...\n");
	listen_sock = socket(AF_INET, SOCK_STREAM, 0);
	memset(&serv_addr, '0', sizeof(serv_addr));

	serv_addr.sin_family = AF_INET;
	serv_addr.sin_addr.s_addr = htonl(INADDR_ANY);
	serv_addr.sin_port = htons(listen_portno);

	bind(listen_sock, (struct sockaddr*)&serv_addr, sizeof(serv_addr));

	listen(listen_sock, 10);

	client_addr_len = sizeof(struct sockaddr_in);
	if ((com_sock = accept(listen_sock, &client_addr, &client_addr_len)) < 0) {
		perror("accept");
		exit(EXIT_FAILURE);
	}
	char buf[INET_ADDRSTRLEN];
	if (inet_ntop(AF_INET, (const void*)&client_addr.sin_addr, buf, INET_ADDRSTRLEN) == NULL) {
		perror("inet_ntop");
		exit(EXIT_FAILURE);
	}
	fprintf(stderr, "[INFO] Incomming migration from: %s\n", buf);

	/* recv migration parameters */
	res = recv_data(&mig_params, sizeof(mig_params_t));
	print_migration_params();
}

/**
 * \brief Receives data from the migration socket
 *
 * \param buffer the destination buffer
 * \param length the buffer size
 */
int recv_data(void *buffer, size_t length)
{
	size_t bytes_received = 0;
	while(bytes_received < length) {
		bytes_received += recv(
				com_sock,
				(void*)((uint64_t)buffer+bytes_received),
				length-bytes_received,
			       	0);
	}

	return bytes_received;
}

/**
 * \brief Sends data via the migration socket
 *
 * \param buffer the source buffer
 * \param length the buffer size
 */
int send_data(void *buffer, size_t length)
{
	size_t bytes_sent = 0;
	while(bytes_sent < length) {
		bytes_sent += send(
				com_sock,
				(void*)((uint64_t)buffer+bytes_sent),
				length-bytes_sent,
			       	0);
	}

	return bytes_sent;
}

/**
 * \brief Closes the TCP connection
 */
void close_migration_channel(void)
{
	if (listen_sock) {
		close_sock(listen_sock);
	}
	close_sock(com_sock);
}


/**
 * \brief Sends the memory regions to be registered at the destination
 *
 * \param guest_physical_memory chunks of the guest-physical memory
 * \param mem_mappings allocated memory regions
 */
void send_mem_regions(mem_mappings_t guest_physical_memory, mem_mappings_t mem_mappings)
{
	/* send to destination */
	if ((mig_params.type == MIG_TYPE_LIVE) || (mem_mappings.count == 0)) {
		send_data(&(guest_physical_memory.count), sizeof(size_t));
		send_data(guest_physical_memory.mem_chunks, guest_physical_memory.count*sizeof(mem_chunk_t));
	} else {
		send_data(&(mem_mappings.count), sizeof(size_t));
		send_data(mem_mappings.mem_chunks, mem_mappings.count*sizeof(mem_chunk_t));
	}
}

/**
 * \brief Receives the memory regions to be registered at the destination
 *
 * \param mem_mappings memory regions to be registered
 */
void recv_mem_regions(mem_mappings_t *mem_mappings)
{
	/* receive the number of memory regions */
	mem_mappings->count = 0;
        mem_mappings->mem_chunks = NULL;
	recv_data(&(mem_mappings->count), sizeof(mem_mappings->count));

	/* receive the region info */
	size_t recv_bytes = mem_mappings->count*sizeof(mem_chunk_t);
	mem_mappings->mem_chunks = (mem_chunk_t*)malloc(recv_bytes);
	recv_data(mem_mappings->mem_chunks, recv_bytes);
}

#else

/* dummy implementation for aarch64 */

void set_migration_target(const char *ip_str, int port)
{
}

void set_migration_type(const char *mig_type_str)
{
}

#endif

